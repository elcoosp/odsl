//! The ODSL pest parser entry module.

use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "grammar.pest"]
pub struct OdslParser;
