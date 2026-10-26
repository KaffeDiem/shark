// Reuse the app's private UI helpers; libtest-only helpers are unused in this main-thread harness.
#![allow(dead_code, unused_imports)]
use core::prelude::v1::test;
include!("../src/main.rs");
