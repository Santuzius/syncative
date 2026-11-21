package io.github.santuzius.syncative;

import android.app.Application;
import android.system.ErrnoException;
import android.system.Os;
import android.util.Log;

import java.io.File;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

/** Runs first in every Syncative process, whether the launcher, the sync service or the boot receiver started it: sets up the environment and loads the native library. */
public class SyncativeApplication extends Application {
    static native void nativeInit(Application application);

    /** Open once onCreate has loaded the native library. */
    private static final CountDownLatch ready = new CountDownLatch(1);

    /** Waits until the native library is loaded: a content provider is published, and may be called on a binder thread, before onCreate runs. */
    static void awaitNative() {
        try {
            ready.await(30, TimeUnit.SECONDS);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
    }

    @Override
    public void onCreate() {
        super.onCreate();

        File files = getFilesDir();
        File cache = new File(files, "cache");
        File run = new File(files, "run");
        cache.mkdirs();
        run.mkdirs();
        // Copies of shared files that a killed process left behind; nothing uses them at its start.
        Bridge.deleteTree(new File(getCacheDir(), Bridge.SHARED_DIR));

        // Syncative, rclone and Go look for their directories here, and Android sets none of them. Set before the library loads: the Go runtime copies the environment when it starts.
        try {
            Os.setenv("HOME", files.getPath(), true);
            Os.setenv("XDG_DATA_HOME", files.getPath(), true);
            Os.setenv("XDG_CONFIG_HOME", files.getPath(), true);
            Os.setenv("XDG_CACHE_HOME", cache.getPath(), true);
            Os.setenv("XDG_RUNTIME_DIR", run.getPath(), true);
            // Go's os.TempDir falls back to /data/local/tmp, which apps cannot write.
            Os.setenv("TMPDIR", cache.getPath(), true);
        } catch (ErrnoException e) {
            Log.e("syncative", "setenv failed", e);
        }

        System.loadLibrary("syncative");
        nativeInit(this);
        ready.countDown();
        Bridge.watchNetwork(this);
    }
}
