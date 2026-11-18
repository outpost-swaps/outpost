extern crate core;
extern crate jni;









mod swap_state_manager;
mod util;
mod resume;
#[cfg(feature = "protocol-v5")]
mod resume_v5;
mod wallet;
mod buy_xmr;
#[cfg(feature = "protocol-v5")]
mod buy_xmr_v5;
mod seller_fee;
mod get_history;
mod model;
mod list_sellers;
mod internal;
mod get_asb_history;

#[no_mangle]
pub extern "system" fn JNI_OnLoad(
    _vm: *mut jni::sys::JavaVM,
    _reserved: *mut std::os::raw::c_void,
) -> jni::sys::jint {
    swap::util::install_default_crypto_provider();
    jni::sys::JNI_VERSION_1_6
}
