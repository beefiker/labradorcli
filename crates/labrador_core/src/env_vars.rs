use std::{env, ffi::OsString};

pub fn var(primary: &str, legacy: &str) -> Option<String> {
    env::var(primary).ok().or_else(|| env::var(legacy).ok())
}

pub fn var_os(primary: &str, legacy: &str) -> Option<OsString> {
    env::var_os(primary).or_else(|| env::var_os(legacy))
}

pub fn is_set(primary: &str, legacy: &str) -> bool {
    var_os(primary, legacy).is_some()
}
