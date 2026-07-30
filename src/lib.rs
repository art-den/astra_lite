#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#![allow(
    clippy::too_many_arguments,
    clippy::upper_case_acronyms,
    clippy::uninlined_format_args,
    clippy::wrong_self_convention,
    clippy::inherent_to_string,
    clippy::single_match,
    clippy::manual_div_ceil,
    clippy::if_same_then_else,
    clippy::module_inception,
    clippy::manual_map,
    clippy::type_complexity,
    clippy::collapsible_else_if,
    clippy::manual_range_contains,
    clippy::collapsible_match,
    clippy::enum_variant_names,
    clippy::large_enum_variant,
    clippy::manual_checked_ops,
)]

pub mod ui;
pub mod utils;
pub mod image;
pub mod hal;
pub mod guiding;
pub mod plate_solve;
pub mod core;
pub mod sky_math;
pub mod options;
