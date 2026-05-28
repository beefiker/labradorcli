//! This module contains Rust types for the GenerateAIInputSuggestions server endpoint that
//! serves Agent Mode.
//!
//! These types are manually transposed from the API schema defined in go
//! (server/model/types/generate_ai_input_suggestions/(request.go|response.go|common.go)).
//!
//! Documentation on the types here is directly borrowed from the documentation on the go schema;
//! see the go schema for the source-of-truth.
mod request;
mod response;

pub use request::*;
pub use response::*;
