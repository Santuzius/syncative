package io.github.santuzius.syncative;

import android.content.Context;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.os.Bundle;
import android.os.CancellationSignal;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.ParcelFileDescriptor;
import android.os.ProxyFileDescriptorCallback;
import android.os.storage.StorageManager;
import android.provider.DocumentsContract;
import android.provider.DocumentsContract.Document;
import android.provider.DocumentsContract.Root;
import android.provider.DocumentsProvider;
import android.system.ErrnoException;
import android.system.OsConstants;
import android.util.Log;
import android.webkit.MimeTypeMap;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.io.FileNotFoundException;
import java.util.ArrayList;
import java.util.List;
import java.util.HashMap;
import java.util.HashSet;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;

/**
 * Shows Syncative's network drive folders to other apps: in the system's Files app and in every app's Open and Save dialog. Each folder is a root; a document's ID is the folder's ID, a slash and the path below it ("14/Photos/a.jpg"; "14" for the folder itself).
 *
 * The work happens in rclone's VFS (src/go/rclone/netdrive/documents.go), reached through the native library (src/infrastructure/android/provider.rs). Open files are proxy file descriptors whose reads and writes go to the VFS; a written file is uploaded when it is closed.
 */
public final class NetworkDriveProvider extends DocumentsProvider {
    static final String AUTHORITY = "io.github.santuzius.syncative.networkdrive";

    /** The network drives, `[{id, title, summary}]`; starts the engine in a process that has none yet. */
    private static native String nativeRootsNow();

    static String nativeRoots() {
        SyncativeApplication.awaitNative();
        return nativeRootsNow();
    }

    /** The RPC call `method` with the JSON object `input` on network drive `root`; the reply, or `{"error":…}`. */
    private static native String nativeCallNow(String method, int root, String input);

    static String nativeCall(String method, int root, String input) {
        SyncativeApplication.awaitNative();
        return nativeCallNow(method, root, input);
    }

    static native int nativeRead(long handle, long offset, int size, byte[] data);

    static native int nativeWrite(long handle, long offset, int size, byte[] data);

    private static final String[] ROOT_COLUMNS = {
            Root.COLUMN_ROOT_ID, Root.COLUMN_DOCUMENT_ID, Root.COLUMN_TITLE, Root.COLUMN_SUMMARY, Root.COLUMN_FLAGS, Root.COLUMN_ICON,
    };
    private static final String[] DOCUMENT_COLUMNS = {
            Document.COLUMN_DOCUMENT_ID, Document.COLUMN_DISPLAY_NAME, Document.COLUMN_MIME_TYPE, Document.COLUMN_SIZE, Document.COLUMN_LAST_MODIFIED, Document.COLUMN_FLAGS,
    };

    /** How long a listing may take before the answer is "loading" and the list follows through notifyChange. */
    private static final long LIST_WAIT_MILLIS = 500;

    private final ExecutorService listings = Executors.newCachedThreadPool();
    /** Folders being listed in the background. */
    private final Set<String> loading = new HashSet<>();
    /** Why a background listing failed, shown once by the next query of that folder. */
    private final Map<String, String> failed = new HashMap<>();

    @Override
    public boolean onCreate() {
        // The native library loads in SyncativeApplication.onCreate, which Android runs after this; nothing native here.
        return true;
    }

    // Document IDs.

    private static int rootOf(String documentId) throws FileNotFoundException {
        int slash = documentId.indexOf('/');
        try {
            return Integer.parseInt(slash < 0 ? documentId : documentId.substring(0, slash));
        } catch (NumberFormatException e) {
            throw new FileNotFoundException("No such document: " + documentId);
        }
    }

    private static String pathOf(String documentId) {
        int slash = documentId.indexOf('/');
        return slash < 0 ? "" : documentId.substring(slash + 1);
    }

    private static String documentId(int root, String path) {
        return path.isEmpty() ? String.valueOf(root) : root + "/" + path;
    }

    private static String join(String parent, String name) {
        return parent.isEmpty() ? name : parent + "/" + name;
    }

    private static String parentOf(String path) {
        int slash = path.lastIndexOf('/');
        return slash < 0 ? "" : path.substring(0, slash);
    }

    // Native calls.

    /** Runs `method` on the drive of `documentId`; throws FileNotFoundException with the error. */
    private static JSONObject call(String method, int root, JSONObject input) throws FileNotFoundException {
        String reply = nativeCall(method, root, input.toString());
        try {
            JSONObject json = new JSONObject(reply);
            if (json.has("error")) {
                throw new FileNotFoundException(json.getString("error"));
            }
            return json;
        } catch (JSONException e) {
            throw new FileNotFoundException("Unreadable answer: " + reply);
        }
    }

    private static JSONObject params(Object... keysAndValues) {
        JSONObject json = new JSONObject();
        try {
            for (int i = 0; i < keysAndValues.length; i += 2) {
                json.put((String) keysAndValues[i], keysAndValues[i + 1]);
            }
        } catch (JSONException e) {
            throw new IllegalArgumentException(e);
        }
        return json;
    }

    private void notifyChildren(int root, String folder) {
        Context context = getContext();
        if (context != null) {
            context.getContentResolver().notifyChange(DocumentsContract.buildChildDocumentsUri(AUTHORITY, documentId(root, folder)), null);
        }
    }

    // Roots.

    @Override
    public Cursor queryRoots(String[] projection) {
        MatrixCursor cursor = new MatrixCursor(projection != null ? projection : ROOT_COLUMNS);
        try {
            JSONArray roots = new JSONArray(nativeRoots());
            for (int i = 0; i < roots.length(); i++) {
                JSONObject root = roots.getJSONObject(i);
                String id = String.valueOf(root.getInt("id"));
                String summary = root.optString("summary");
                cursor.newRow()
                        .add(Root.COLUMN_ROOT_ID, id)
                        .add(Root.COLUMN_DOCUMENT_ID, id)
                        .add(Root.COLUMN_TITLE, root.getString("title"))
                        .add(Root.COLUMN_SUMMARY, summary.isEmpty() ? "Syncative" : summary)
                        .add(Root.COLUMN_FLAGS, Root.FLAG_SUPPORTS_CREATE | Root.FLAG_SUPPORTS_IS_CHILD)
                        .add(Root.COLUMN_ICON, R.mipmap.ic_launcher);
            }
        } catch (JSONException e) {
            Log.e(Bridge.TAG, "network drive roots", e);
        }
        cursor.setNotificationUri(getContext().getContentResolver(), DocumentsContract.buildRootsUri(AUTHORITY));
        return cursor;
    }

    // Listing.

    private static void addRow(MatrixCursor cursor, String documentId, JSONObject entry, String name) throws JSONException {
        boolean dir = entry.getBoolean("dir");
        boolean top = documentId.indexOf('/') < 0;
        int flags;
        if (dir) {
            flags = Document.FLAG_DIR_SUPPORTS_CREATE;
            if (!top) {
                flags |= Document.FLAG_SUPPORTS_DELETE | Document.FLAG_SUPPORTS_RENAME | Document.FLAG_SUPPORTS_MOVE;
            }
        } else {
            flags = Document.FLAG_SUPPORTS_WRITE | Document.FLAG_SUPPORTS_DELETE | Document.FLAG_SUPPORTS_RENAME | Document.FLAG_SUPPORTS_MOVE;
        }
        cursor.newRow()
                .add(Document.COLUMN_DOCUMENT_ID, documentId)
                .add(Document.COLUMN_DISPLAY_NAME, name)
                .add(Document.COLUMN_MIME_TYPE, dir ? Document.MIME_TYPE_DIR : mimeType(name))
                .add(Document.COLUMN_SIZE, dir ? null : entry.getLong("size"))
                .add(Document.COLUMN_LAST_MODIFIED, entry.getLong("modified"))
                .add(Document.COLUMN_FLAGS, flags);
    }

    private static String mimeType(String name) {
        int dot = name.lastIndexOf('.');
        if (dot >= 0) {
            String type = MimeTypeMap.getSingleton().getMimeTypeFromExtension(name.substring(dot + 1).toLowerCase(Locale.ROOT));
            if (type != null) {
                return type;
            }
        }
        return "application/octet-stream";
    }

    @Override
    public Cursor queryDocument(String documentId, String[] projection) throws FileNotFoundException {
        int root = rootOf(documentId);
        String path = pathOf(documentId);
        JSONObject entry = call("syncative/doc-stat", root, params("path", path));
        MatrixCursor cursor = new MatrixCursor(projection != null ? projection : DOCUMENT_COLUMNS);
        try {
            String name = path.isEmpty() ? rootTitle(root) : entry.getString("name");
            addRow(cursor, documentId, entry, name);
        } catch (JSONException e) {
            throw new FileNotFoundException(e.getMessage());
        }
        return cursor;
    }

    private static String rootTitle(int root) {
        try {
            JSONArray roots = new JSONArray(nativeRoots());
            for (int i = 0; i < roots.length(); i++) {
                if (roots.getJSONObject(i).getInt("id") == root) {
                    return roots.getJSONObject(i).getString("title");
                }
            }
        } catch (JSONException ignored) {
        }
        return "Syncative";
    }

    @Override
    public Cursor queryChildDocuments(String parentDocumentId, String[] projection, String sortOrder) throws FileNotFoundException {
        int root = rootOf(parentDocumentId);
        String path = pathOf(parentDocumentId);
        MatrixCursor cursor = new MatrixCursor(projection != null ? projection : DOCUMENT_COLUMNS);
        Uri uri = DocumentsContract.buildChildDocumentsUri(AUTHORITY, parentDocumentId);
        cursor.setNotificationUri(getContext().getContentResolver(), uri);

        String error;
        synchronized (this) {
            error = failed.remove(parentDocumentId);
            if (error == null && loading.contains(parentDocumentId)) {
                return loadingCursor(cursor);
            }
        }
        if (error != null) {
            Bundle extras = new Bundle();
            extras.putString(DocumentsContract.EXTRA_ERROR, error);
            cursor.setExtras(extras);
            return cursor;
        }

        // A listing the VFS has cached answers at once; one from the remote may take seconds, and the folder then shows "loading" until notifyChange.
        Future<String> listing = listings.submit(() -> nativeCall("syncative/doc-list", root, params("path", path).toString()));
        String reply;
        try {
            reply = listing.get(LIST_WAIT_MILLIS, TimeUnit.MILLISECONDS);
        } catch (TimeoutException e) {
            synchronized (this) {
                loading.add(parentDocumentId);
            }
            listings.submit(() -> {
                String error1 = null;
                try {
                    JSONObject json = new JSONObject(listing.get());
                    error1 = json.has("error") ? json.getString("error") : null;
                } catch (Exception e1) {
                    error1 = String.valueOf(e1.getMessage());
                }
                synchronized (NetworkDriveProvider.this) {
                    loading.remove(parentDocumentId);
                    if (error1 != null) {
                        failed.put(parentDocumentId, error1);
                    }
                }
                getContext().getContentResolver().notifyChange(uri, null);
            });
            return loadingCursor(cursor);
        } catch (Exception e) {
            throw new FileNotFoundException(String.valueOf(e.getMessage()));
        }
        try {
            JSONObject json = new JSONObject(reply);
            if (json.has("error")) {
                throw new FileNotFoundException(json.getString("error"));
            }
            JSONArray entries = json.getJSONArray("entries");
            for (int i = 0; i < entries.length(); i++) {
                JSONObject entry = entries.getJSONObject(i);
                String name = entry.getString("name");
                addRow(cursor, documentId(root, join(path, name)), entry, name);
            }
        } catch (JSONException e) {
            throw new FileNotFoundException(e.getMessage());
        }
        return cursor;
    }

    private static Cursor loadingCursor(MatrixCursor cursor) {
        Bundle extras = new Bundle();
        extras.putBoolean(DocumentsContract.EXTRA_LOADING, true);
        cursor.setExtras(extras);
        return cursor;
    }

    @Override
    public boolean isChildDocument(String parentDocumentId, String documentId) {
        try {
            if (rootOf(parentDocumentId) != rootOf(documentId)) {
                return false;
            }
        } catch (FileNotFoundException e) {
            return false;
        }
        String parent = pathOf(parentDocumentId);
        String path = pathOf(documentId);
        return parent.isEmpty() ? !path.isEmpty() : path.startsWith(parent + "/");
    }

    /** The documents from the drive's top (or `parentDocumentId`) down to `childDocumentId`, so the Files app can open a folder from a link, e.g. the drive itself from Syncative. */
    @Override
    public DocumentsContract.Path findDocumentPath(String parentDocumentId, String childDocumentId) throws FileNotFoundException {
        int root = rootOf(childDocumentId);
        String path = pathOf(childDocumentId);
        List<String> ids = new ArrayList<>();
        ids.add(documentId(root, ""));
        if (!path.isEmpty()) {
            String prefix = "";
            for (String part : path.split("/")) {
                prefix = join(prefix, part);
                ids.add(documentId(root, prefix));
            }
        }
        if (parentDocumentId != null) {
            int start = ids.indexOf(parentDocumentId);
            if (start < 0) {
                throw new FileNotFoundException(childDocumentId + " isn't in " + parentDocumentId);
            }
            return new DocumentsContract.Path(null, ids.subList(start, ids.size()));
        }
        return new DocumentsContract.Path(String.valueOf(root), ids);
    }

    // Changes.

    @Override
    public String createDocument(String parentDocumentId, String mimeType, String displayName) throws FileNotFoundException {
        int root = rootOf(parentDocumentId);
        String parent = pathOf(parentDocumentId);
        boolean dir = Document.MIME_TYPE_DIR.equals(mimeType);
        String path = freePath(root, parent, displayName);
        call(dir ? "syncative/doc-mkdir" : "syncative/doc-create", root, params("path", path));
        notifyChildren(root, parent);
        return documentId(root, path);
    }

    /** `name` in `parent`, or "name (1).ext" and so on when that is taken, as Android expects of a provider. */
    private static String freePath(int root, String parent, String name) throws FileNotFoundException {
        int dot = name.lastIndexOf('.');
        String base = dot > 0 ? name.substring(0, dot) : name;
        String extension = dot > 0 ? name.substring(dot) : "";
        for (int i = 0; i < 100; i++) {
            String candidate = join(parent, i == 0 ? name : base + " (" + i + ")" + extension);
            if (call("syncative/doc-stat", root, params("path", candidate, "probe", true)).optBoolean("missing")) {
                return candidate;
            }
        }
        throw new FileNotFoundException("No free name for " + name);
    }

    @Override
    public void deleteDocument(String documentId) throws FileNotFoundException {
        int root = rootOf(documentId);
        String path = pathOf(documentId);
        call("syncative/doc-remove", root, params("path", path));
        revokeDocumentPermission(documentId);
        notifyChildren(root, parentOf(path));
    }

    @Override
    public String renameDocument(String documentId, String displayName) throws FileNotFoundException {
        int root = rootOf(documentId);
        String path = pathOf(documentId);
        String target = join(parentOf(path), displayName);
        call("syncative/doc-rename", root, params("from", path, "to", target));
        revokeDocumentPermission(documentId);
        notifyChildren(root, parentOf(path));
        return documentId(root, target);
    }

    @Override
    public String moveDocument(String sourceDocumentId, String sourceParentDocumentId, String targetParentDocumentId) throws FileNotFoundException {
        int root = rootOf(sourceDocumentId);
        if (root != rootOf(targetParentDocumentId)) {
            // Between two drives DocumentsUI copies and deletes instead.
            throw new UnsupportedOperationException("Moving between network drives");
        }
        String path = pathOf(sourceDocumentId);
        String name = path.substring(path.lastIndexOf('/') + 1);
        String target = join(pathOf(targetParentDocumentId), name);
        call("syncative/doc-rename", root, params("from", path, "to", target));
        revokeDocumentPermission(sourceDocumentId);
        notifyChildren(root, parentOf(path));
        notifyChildren(root, pathOf(targetParentDocumentId));
        return documentId(root, target);
    }

    // File contents.

    @Override
    public ParcelFileDescriptor openDocument(String documentId, String mode, CancellationSignal signal) throws FileNotFoundException {
        int root = rootOf(documentId);
        String path = pathOf(documentId);
        int pfdMode = ParcelFileDescriptor.parseMode(mode);
        int access = pfdMode & ParcelFileDescriptor.MODE_READ_WRITE;
        int flags;
        if (access == ParcelFileDescriptor.MODE_READ_WRITE) {
            flags = OsConstants.O_RDWR;
        } else if (access == ParcelFileDescriptor.MODE_WRITE_ONLY) {
            flags = OsConstants.O_WRONLY;
        } else {
            flags = OsConstants.O_RDONLY;
        }
        if ((pfdMode & ParcelFileDescriptor.MODE_CREATE) != 0) {
            flags |= OsConstants.O_CREAT;
        }
        if ((pfdMode & ParcelFileDescriptor.MODE_TRUNCATE) != 0) {
            flags |= OsConstants.O_TRUNC;
        }
        if ((pfdMode & ParcelFileDescriptor.MODE_APPEND) != 0) {
            flags |= OsConstants.O_APPEND;
        }
        boolean writing = access != ParcelFileDescriptor.MODE_READ_ONLY;
        long handle = call("syncative/doc-open", root, params("path", path, "flags", flags)).optLong("handle");

        // A thread per open file: Android waits for onGetSize while it holds a lock of vold's, and a slow read of another file on a shared thread could keep it waiting long enough for the watchdog to restart the system (as RSAF found, github.com/chenxiaolong/RSAF).
        HandlerThread thread = new HandlerThread("syncative-file");
        thread.start();
        OpenFile file = new OpenFile(getContext(), root, path, handle, writing, thread);
        try {
            StorageManager storage = getContext().getSystemService(StorageManager.class);
            return storage.openProxyFileDescriptor(access, file, new Handler(thread.getLooper()));
        } catch (Exception e) {
            file.close();
            throw new FileNotFoundException(String.valueOf(e.getMessage()));
        }
    }

    private final class OpenFile extends ProxyFileDescriptorCallback {
        private final Context context;
        private final int root;
        private final String path;
        private final long handle;
        private final boolean writing;
        private final HandlerThread thread;

        OpenFile(Context context, int root, String path, long handle, boolean writing, HandlerThread thread) {
            this.context = context;
            this.root = root;
            this.path = path;
            this.handle = handle;
            this.writing = writing;
            this.thread = thread;
        }

        private JSONObject onHandle(String method) throws ErrnoException {
            try {
                return call(method, -1, params("handle", handle));
            } catch (FileNotFoundException e) {
                throw new ErrnoException(method + ": " + e.getMessage(), OsConstants.EIO);
            }
        }

        @Override
        public long onGetSize() throws ErrnoException {
            return onHandle("syncative/doc-size").optLong("size");
        }

        @Override
        public int onRead(long offset, int size, byte[] data) throws ErrnoException {
            int n = nativeRead(handle, offset, size, data);
            if (n < 0) {
                throw new ErrnoException("read", OsConstants.EIO);
            }
            return n;
        }

        @Override
        public int onWrite(long offset, int size, byte[] data) throws ErrnoException {
            int n = nativeWrite(handle, offset, size, data);
            if (n < 0) {
                throw new ErrnoException("write", OsConstants.EIO);
            }
            return n;
        }

        @Override
        public void onFsync() throws ErrnoException {
            onHandle("syncative/doc-flush");
        }

        @Override
        public void onRelease() {
            // Closing a written file uploads it, which may take long; this callback must not wait for it (see openDocument).
            new Thread(this::close).start();
        }

        void close() {
            thread.quitSafely();
            String reply = nativeCall("syncative/doc-close", -1, params("handle", handle).toString());
            if (writing) {
                if (reply.contains("\"error\"")) {
                    String error;
                    try {
                        error = new JSONObject(reply).getString("error");
                    } catch (JSONException e) {
                        error = reply;
                    }
                    Bridge.notify(context, "Couldn't upload " + path.substring(path.lastIndexOf('/') + 1), error);
                }
                notifyChildren(root, parentOf(path));
            }
        }
    }
}
