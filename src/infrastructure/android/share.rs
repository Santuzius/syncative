//! What sharing from other apps needs from Android: going back to the app the files came from, and a toast. The files themselves arrive through `src/inbound/android.rs`.

use jni::objects::JValue;

use super::{bridge, with_context};

/// Back to the app the files came from.
pub fn return_to_caller() {
    with_context(|env, context| {
        let class = bridge(env, context)?;
        env.call_static_method(class, "returnToCaller", "()V", &[])?;
        Ok(())
    });
}

/// A short message at the bottom of the screen.
pub fn toast(text: &str) {
    with_context(|env, context| {
        let class = bridge(env, context)?;
        let text = env.new_string(text)?;
        env.call_static_method(
            class,
            "toast",
            "(Landroid/content/Context;Ljava/lang/String;)V",
            &[JValue::Object(context), JValue::Object(&text)],
        )?;
        Ok(())
    });
}
