// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! Committee Notification Listener
//!
//! Previously contained a Unix socket–based push notification listener for
//! committee changes from Go Master. This approach was replaced by the unified
//! epoch monitor (`epoch_monitor.rs`), which polls Go and peers directly.
//!
//! The module declaration is kept in `mod.rs` to avoid breaking `pub mod` imports.
