//! Files shared from other apps: `Bridge.receiveShare` copies them to the cache and reports through `nativeShareStarted` and `nativeShareReady`; the window takes them from [`crate::services::share`].

use std::path::PathBuf;

use jni::{
    objects::{JClass, JObjectArray, JString},
    sys::jint,
    JNIEnv,
};

use super::{bridge, with_context};
use crate::services::share::{self, Incoming};

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_santuzius_syncative_Bridge_nativeShareStarted(mut env: JNIEnv, _: JClass, folder: JString, count: jint) {
    let Ok(folder) = env.get_string(&folder).map(String::from) else {
        return;
    };
    share::push(Incoming::Started { folder: PathBuf::from(folder), count: count.max(0) as usize });
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_santuzius_syncative_Bridge_nativeShareReady(mut env: JNIEnv, _: JClass, folder: JString, paths: JObjectArray, error: JString) {
    let Ok(folder) = env.get_string(&folder).map(String::from) else {
        return;
    };
    let mut files = Vec::new();
    let count = env.get_array_length(&paths).unwrap_or(0);
    for i in 0..count {
        let Ok(path) = env.get_object_array_element(&paths, i) else {
            continue;
        };
        if let Ok(path) = env.get_string(&JString::from(path)) {
            files.push(PathBuf::from(String::from(path)));
        }
    }
    let error = if error.is_null() { None } else { env.get_string(&error).ok().map(String::from) };
    share::push(Incoming::Ready { folder: PathBuf::from(folder), files, error });
}

/// Back to the app the files came from.
pub fn return_to_caller() {
    with_context(|env, context| {
        let class = bridge(env, context)?;
        env.call_static_method(class, "returnToCaller", "()V", &[])?;
        Ok(())
    });
}
