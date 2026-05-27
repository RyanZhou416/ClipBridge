#![warn(clippy::pedantic)]
#![allow(
    clippy::tabs_in_doc_comments,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::doc_markdown,
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::struct_field_names,
    clippy::if_not_else,
    clippy::used_underscore_binding,
    clippy::module_name_repetitions,
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

pub mod api;
pub mod cas;
pub mod clipboard;
pub mod crypto;
pub mod discovery;
pub mod logs;
pub mod model;
pub mod net;
pub mod policy;
pub mod prelude;
pub mod proto;
pub mod runtime;
pub mod session;
pub mod stats;
pub mod store;
pub mod testsupport;
pub mod transport;
pub mod util;
