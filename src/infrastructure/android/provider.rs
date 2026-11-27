//! The native half of `NetworkDriveProvider`, the document provider that shows network drive folders to other apps. The Java side turns Android's document calls into the RPC calls of src/go/rclone/netdrive/documents.go; file contents go through [`syncative_go::doc_read`] and [`syncative_go::doc_write`].

use std::sync::Mutex;

use jni::{
    JNIEnv,
    objects::{JByteArray, JClass, JString, JValue},
    sys::{jint, jlong, jstring},
};

/// A network drive as Android's document provider offers it to other apps.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Root {
    /// The folder's ID, which the provider's document IDs start with.
    pub id: i32,
    /// What the VFS shows, `remote:path`.
    #[serde(skip)]
    pub fs: String,
    /// The stand-in local path, see [`SyncDir::android_drive_path`](crate::domain::sync::SyncDir::android_drive_path).
    #[serde(skip)]
    pub local_path: String,
    /// The folder on the remote, or the remote's name for the whole drive.
    pub title: String,
    /// The remote's name; empty for the whole drive, whose title already names it.
    pub summary: String,
}

static ROOTS: Mutex<Vec<Root>> = Mutex::new(Vec::new());

/// The network drives the document provider offers, as the engine last set them, in JSON `[{id, title, summary}]`.
pub fn roots_json() -> String {
    let roots = ROOTS.lock().map(|r| r.clone()).unwrap_or_default();
    serde_json::to_string(&roots).unwrap_or_else(|_| "[]".to_owned())
}

/// `remote:path` of the network drive folder `id`, for the document provider.
fn root_fs(id: i32) -> Option<String> {
    ROOTS
        .lock()
        .ok()?
        .iter()
        .find(|r| r.id == id)
        .map(|r| r.fs.clone())
}

/// The ID of the network drive whose stand-in local path is `local_path`, see [`SyncDir::android_drive_path`](crate::domain::sync::SyncDir::android_drive_path).
pub(super) fn root_at(local_path: &str) -> Option<i32> {
    ROOTS
        .lock()
        .ok()?
        .iter()
        .find(|r| r.local_path == local_path)
        .map(|r| r.id)
}

/// Sets the network drives the document provider offers and tells apps showing it when they changed. The engine calls it after reading the folders.
pub fn set_roots(roots: Vec<Root>) {
    let Ok(mut current) = ROOTS.lock() else {
        return;
    };
    let changed = current.len() != roots.len()
        || current
            .iter()
            .zip(&roots)
            .any(|(a, b)| a.id != b.id || a.fs != b.fs);
    *current = roots;
    drop(current);
    if changed {
        roots_changed();
    }
}

fn new_string(env: &mut JNIEnv, text: &str) -> jstring {
    env.new_string(text)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

fn error_json(text: &str) -> String {
    serde_json::json!({ "error": text }).to_string()
}

/// Runs the RPC call `method` (`syncative/doc-…`) with `input` (a JSON object) on network drive `root`, whose `fs` it adds. Returns the reply, or `{"error":…}`.
#[unsafe(no_mangle)]
extern "system" fn Java_io_github_santuzius_syncative_NetworkDriveProvider_nativeCallNow(
    mut env: JNIEnv,
    _: JClass,
    method: JString,
    root: jint,
    input: JString,
) -> jstring {
    let reply = (|| {
        let method: String = env.get_string(&method).map_err(|e| e.to_string())?.into();
        let input: String = env.get_string(&input).map_err(|e| e.to_string())?.into();
        let mut input: serde_json::Value =
            serde_json::from_str(&input).map_err(|e| e.to_string())?;
        if root >= 0 {
            let fs = root_fs(root).ok_or("This network drive was removed.")?;
            input["fs"] = fs.into();
        }
        syncative_go::rpc(method, input.to_string()).map_err(|e| {
            serde_json::from_str::<serde_json::Value>(&e)
                .ok()
                .and_then(|v| v.get("error")?.as_str().map(str::to_owned))
                .unwrap_or(e)
        })
    })();
    let json = reply.unwrap_or_else(|e: String| error_json(&e));
    new_string(&mut env, &json)
}

/// Reads up to `size` bytes at `offset` of the open file `handle` into `data`: how many, or -1.
#[unsafe(no_mangle)]
extern "system" fn Java_io_github_santuzius_syncative_NetworkDriveProvider_nativeRead(
    env: JNIEnv,
    _: JClass,
    handle: jlong,
    offset: jlong,
    size: jint,
    data: JByteArray,
) -> jint {
    let mut buf = vec![0u8; usize::try_from(size).unwrap_or(0)];
    let Some(n) = syncative_go::doc_read(handle, offset, &mut buf) else {
        return -1;
    };
    // SAFETY: u8 and i8 have the same size and alignment.
    let bytes: &[i8] = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast(), n) };
    if env.set_byte_array_region(&data, 0, bytes).is_err() {
        return -1;
    }
    n as jint
}

/// Writes `size` bytes of `data` at `offset` of the open file `handle`: how many, or -1.
#[unsafe(no_mangle)]
extern "system" fn Java_io_github_santuzius_syncative_NetworkDriveProvider_nativeWrite(
    env: JNIEnv,
    _: JClass,
    handle: jlong,
    offset: jlong,
    size: jint,
    data: JByteArray,
) -> jint {
    let mut buf = vec![0i8; usize::try_from(size).unwrap_or(0)];
    if env.get_byte_array_region(&data, 0, &mut buf).is_err() {
        return -1;
    }
    // SAFETY: u8 and i8 have the same size and alignment.
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast(), buf.len()) };
    syncative_go::doc_write(handle, offset, bytes).map_or(-1, |n| n as jint)
}

/// Tells apps showing Syncative's document provider that the network drives changed, e.g. after one was added.
fn roots_changed() {
    super::with_context(|env, context| {
        let class = super::bridge(env, context)?;
        env.call_static_method(
            class,
            "networkDrivesChanged",
            "(Landroid/content/Context;)V",
            &[JValue::Object(context)],
        )?;
        Ok(())
    });
}
