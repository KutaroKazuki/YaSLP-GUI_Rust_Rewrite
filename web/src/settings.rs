// Settings persistence lives in yaslp-shared; re-export so the web crate can
// call `crate::settings::load()` / `crate::settings::save()` unchanged.
pub use yaslp_shared::settings::{load, save};
