//! The OSDL pest parser entry module.

use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "grammar.pest"]
pub struct OsdlParser;
