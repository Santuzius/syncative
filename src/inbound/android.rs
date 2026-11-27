//! The JNI entry points through which Android's Java side reports to Syncative or asks it to act: the sync service, the network watch, files shared from other apps and the document provider's list of network drives. Each hands what it receives to the service or the engine that handles it. Entry points that only answer a call Syncative made (a picked folder or document) or only reach the Go library stay with their adapters in [`crate::infrastructure::android`].

use std::path::PathBuf;

use jni::{
    JNIEnv,
    objects::{JClass, JObjectArray, JString},
    sys::{JNI_TRUE, jboolean, jint, jstring},
};

use crate::{
    infrastructure::android::provider,
    services::share::{self, Incoming},
};

fn new_string(env: &mut JNIEnv, text: &str) -> jstring {
    env.new_string(text)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Called by `SyncService.onCreate`: starts the sync engine without a GUI, unless it already runs.
#[unsafe(no_mangle)]
extern "system" fn Java_io_github_santuzius_syncative_SyncService_nativeStartEngine(
    _: JNIEnv,
    _: JClass,
) {
    // Setting up opens the database and resumes sessions over the network; keep that off the service's main thread.
    std::thread::spawn(crate::start_engine);
}

/// Called by `SyncService` when "Sync all now" in its notification is tapped.
#[unsafe(no_mangle)]
extern "system" fn Java_io_github_santuzius_syncative_SyncService_nativeSyncAll(
    _: JNIEnv,
    _: JClass,
) {
    if let Some(engine) = crate::engine::get() {
        engine.send(crate::engine::Command::SyncAll);
    }
}

/// Called by `SyncService` when the screen goes off or on, the charger is plugged in or out, or Android's battery saver toggles: the power mode decides how often to sync (see [`crate::services::preferences::power`]).
#[unsafe(no_mangle)]
extern "system" fn Java_io_github_santuzius_syncative_SyncService_nativeConditions(
    _: JNIEnv,
    _: JClass,
    screen_off: jboolean,
    charging: jboolean,
    battery_saver: jboolean,
) {
    crate::services::preferences::power::set_device(
        screen_off == JNI_TRUE,
        charging == JNI_TRUE,
        battery_saver == JNI_TRUE,
    );
}

/// Called by `Bridge.watchNetwork` in every process when the default network changes and whether it is metered: Preferences may hold syncing on metered networks.
#[unsafe(no_mangle)]
extern "system" fn Java_io_github_santuzius_syncative_Bridge_nativeMetered(
    _: JNIEnv,
    _: JClass,
    metered: jboolean,
) {
    crate::services::network::set_metered(metered == JNI_TRUE);
}

/// Called by `Bridge.watchNetwork` in every process when the device gets or loses a network below any VPN.
#[unsafe(no_mangle)]
extern "system" fn Java_io_github_santuzius_syncative_Bridge_nativeOnline(
    _: JNIEnv,
    _: JClass,
    online: jboolean,
) {
    crate::services::network::set_online(online == JNI_TRUE);
}

/// Called by `Bridge.receiveShare` once it starts copying `count` shared files to the cache folder `folder`.
#[unsafe(no_mangle)]
extern "system" fn Java_io_github_santuzius_syncative_Bridge_nativeShareStarted(
    mut env: JNIEnv,
    _: JClass,
    folder: JString,
    count: jint,
) {
    let Ok(folder) = env.get_string(&folder).map(String::from) else {
        return;
    };
    share::push(Incoming::Started {
        folder: PathBuf::from(folder),
        count: count.max(0) as usize,
        copies: true,
    });
}

/// Called by `Bridge.receiveShare` when the files are copied, or with `error` when copying failed.
#[unsafe(no_mangle)]
extern "system" fn Java_io_github_santuzius_syncative_Bridge_nativeShareReady(
    mut env: JNIEnv,
    _: JClass,
    folder: JString,
    paths: JObjectArray,
    error: JString,
) {
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
    let error = if error.is_null() {
        None
    } else {
        env.get_string(&error).ok().map(String::from)
    };
    share::push(Incoming::Ready {
        folder: PathBuf::from(folder),
        files,
        error,
    });
}

/// The network drives as JSON, `[{id, title, summary}]`. A process that the provider started has no engine yet; it starts here and announces the drives through `roots_changed` once it has read them.
#[unsafe(no_mangle)]
extern "system" fn Java_io_github_santuzius_syncative_NetworkDriveProvider_nativeRootsNow(
    mut env: JNIEnv,
    _: JClass,
) -> jstring {
    if crate::engine::get().is_none() {
        std::thread::spawn(crate::start_engine);
    }
    new_string(&mut env, &provider::roots_json())
}
