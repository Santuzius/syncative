//! The native half of `NetworkDriveProvider`, the document provider that shows network drive folders ([`crate::services::network_drive`]) to other apps. The Java side turns Android's document calls into the RPC calls of src/go/rclone/netdrive/documents.go; file contents go through [`syncative_go::doc_read`] and [`syncative_go::doc_write`].

use jni::{
    JNIEnv,
    objects::{JByteArray, JClass, JString, JValue},
    sys::{jint, jlong, jstring},
};

use crate::services::network_drive;

fn new_string(env: &mut JNIEnv, text: &str) -> jstring {
    env.new_string(text)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

fn error_json(text: &str) -> String {
    serde_json::json!({ "error": text }).to_string()
}

/// The network drives as JSON, `[{id, title, summary}]`. A process that the provider started has no engine yet; it starts here and announces the drives through [`roots_changed`] once it has read them.
#[unsafe(no_mangle)]
extern "system" fn Java_io_github_santuzius_syncative_NetworkDriveProvider_nativeRootsNow(
    mut env: JNIEnv,
    _: JClass,
) -> jstring {
    if crate::engine::get().is_none() {
        std::thread::spawn(crate::start_engine);
    }
    let json = serde_json::to_string(&network_drive::roots()).unwrap_or_else(|_| "[]".to_owned());
    new_string(&mut env, &json)
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
            let fs = network_drive::root_fs(root).ok_or("This network drive was removed.")?;
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
pub fn roots_changed() {
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
