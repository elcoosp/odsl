//! The OSDL parser: pest token stream -> validated [`Ast`].
//!
//! The pipeline is: [`parse`] produces a raw, arena-populated [`Ast`] (with
//! unresolved references left as `FieldType::InferredRef` / word types), then
//! runs type inference so the returned AST is ready for the validator or
//! renderers (REQ-FUNC-002).

pub mod infer;

use infer::infer_field_type;
use osdl_core::ast::{Ast, Field, Model};
use osdl_core::errors::{OsdlError, ParseError, Span};
use osdl_core::types::{FieldType, Intent, Reference, ScalarType};
use pest::iterators::{Pair, Pairs};
use pest::Parser;
use std::collections::HashSet;

pub mod grammar {
    use pest_derive::Parser;
    #[derive(Parser)]
    #[grammar = "grammar.pest"]
    pub struct OsdlParser;
}

pub use grammar::Rule;

#[derive(Debug, Clone)]
enum RawToken {
    Name(String),
    Flag(String),
    Reference { model: String, field: String },
    Word(String),
}

struct ParsedLine {
    indent: usize,
    tokens: Vec<RawToken>,
    line_no: usize,
    byte_start: usize,
    byte_end: usize,
}

/// Parse OSDL source into a resolved [`Ast`] (inference applied).
pub fn parse_and_resolve(src: &str) -> Result<Ast, OsdlError> {
    parse(src)
}

/// Parse OSDL source into a resolved [`Ast`].
pub fn parse(src: &str) -> Result<Ast, OsdlError> {
    let pairs = grammar::OsdlParser::parse(Rule::file, src).map_err(|e| {
        OsdlError::Parse(ParseError::new(format!("parse error: {e}")))
    })?;

    let mut known_models: HashSet<String> = HashSet::new();
    let parsed = collect_lines(pairs, src)?;
    for line in &parsed {
        if line.indent == 0 && !line.tokens.is_empty() {
            if let Some(RawToken::Name(n)) = line.tokens.first() {
                known_models.insert(n.clone());
            }
        }
    }

    let mut ast = Ast::new();
    let mut current_model: Option<Model> = None;

    for pl in &parsed {
        if pl.indent == 0 {
            if let Some(m) = current_model.take() {
                ast.add_model(m);
            }
            let name = match pl.tokens.first() {
                Some(RawToken::Name(n)) => n.clone(),
                _ => {
                    return Err(OsdlError::Parse(ParseError::new(
                        "expected a model name at column 0",
                    )
                    .with_span(span_at(src, pl.byte_start, pl.byte_end, pl.line_no), src)))
                }
            };
            let mut model = Model {
                name,
                fields: la_arena::Arena::new(),
                field_index: vec![],
                line: pl.line_no,
            };
            let field_tokens: Vec<_> = pl.tokens.iter().skip(1).cloned().collect();
            if !field_tokens.is_empty() {
                add_field_from_tokens(
                    &mut model,
                    &field_tokens,
                    &known_models,
                    pl.line_no,
                    pl.byte_start,
                    pl.byte_end,
                    src,
                )?;
            }
            current_model = Some(model);
        } else {
            let m = match current_model.as_mut() {
                Some(m) => m,
                None => {
                    return Err(OsdlError::Parse(ParseError::new(
                        "field is not inside any model (indented line without a parent model)",
                    )
                    .with_span(span_at(src, pl.byte_start, pl.byte_end, pl.line_no), src)))
                }
            };
            add_field_from_tokens(
                m,
                &pl.tokens,
                &known_models,
                pl.line_no,
                pl.byte_start,
                pl.byte_end,
                src,
            )?;
        }
    }
    if let Some(m) = current_model.take() {
        ast.add_model(m);
    }

    Ok(ast)
}

fn add_field_from_tokens(
    model: &mut Model,
    tokens: &[RawToken],
    known_models: &HashSet<String>,
    line_no: usize,
    byte_start: usize,
    byte_end: usize,
    src: &str,
) -> Result<(), OsdlError> {
    let mut iter = tokens.iter().peekable();

    let name = match iter.peek() {
        Some(RawToken::Name(n)) | Some(RawToken::Word(n)) => {
            let n = n.clone();
            iter.next();
            n
        }
        _ => {
            return Err(OsdlError::Parse(
                ParseError::new("expected a field name")
                    .with_span(span_at(src, byte_start, byte_end, line_no), src),
            ))
        }
    };

    let mut ty: Option<FieldType> = None;
    let mut intents: Vec<Intent> = Vec::new();

    while let Some(tok) = iter.next() {
        match tok {
            RawToken::Flag(f) => {
                let intent = parse_intent(f).ok_or_else(|| {
                    OsdlError::Parse(
                        ParseError::new(format!("unknown intent flag `{f}`"))
                            .with_span(span_at(src, byte_start, byte_end, line_no), src),
                    )
                })?;
                intents.push(intent);
            }
            RawToken::Reference { model: rm, field: rf } => {
                ty = Some(FieldType::Ref(Reference {
                    model: rm.clone(),
                    field: rf.clone(),
                }));
            }
            RawToken::Word(w) => {
                if let Some(target) = w.strip_prefix("relation:") {
                    intents.push(Intent::Relation);
                    ty = Some(FieldType::InferredRef(format!("relation:{target}")));
                } else if let Some(s) = ScalarType::from_keyword(w) {
                    ty = Some(FieldType::Scalar(s));
                } else if known_models.contains(w) {
                    ty = Some(FieldType::Ref(Reference {
                        model: w.clone(),
                        field: "id".into(),
                    }));
                } else {
                    ty = Some(FieldType::InferredRef(w.clone()));
                }
            }
            RawToken::Name(_) => unreachable!("name only appears as first token"),
        }
    }

    let ty = match ty {
        Some(t) => t,
        None => infer_field_type(&name, known_models, &model.name),
    };

    model.add_field(Field {
        name,
        ty,
        intents,
        line: line_no,
    });
    Ok(())
}

fn parse_intent(flag: &str) -> Option<Intent> {
    match flag {
        "-pk" | "-primary" => Some(Intent::Pk),
        "-partition" => Some(Intent::Partition),
        "-uniq" | "-unique" => Some(Intent::Uniq),
        "-null" | "-nullable" => Some(Intent::Null),
        "-fulltext" => Some(Intent::Fulltext),
        "-tz" | "-timezone" => Some(Intent::Tz),
        "-auto" | "-autoincrement" => Some(Intent::Auto),
        "-relation" => Some(Intent::Relation),
        _ => None,
    }
}

fn collect_lines(pairs: Pairs<Rule>, src: &str) -> Result<Vec<ParsedLine>, OsdlError> {
    let mut lines = Vec::new();
    // `parse` yields a single top-level `file` pair; descend into it.
    for pair in pairs {
        if pair.as_rule() == Rule::file {
            for inner in pair.into_inner() {
                match inner.as_rule() {
                    Rule::line => lines.push(parse_line(inner, src)?),
                    Rule::EOI => {}
                    _ => {}
                }
            }
        } else if pair.as_rule() == Rule::line {
            lines.push(parse_line(pair, src)?);
        }
    }
    Ok(lines)
}

fn parse_line(pair: Pair<Rule>, src: &str) -> Result<ParsedLine, OsdlError> {
    let line_no = pair.line_col().0;
    let span = pair.as_span();
    let byte_start = span.start();
    let byte_end = span.end();

    let mut indent = 0usize;
    let mut tokens = Vec::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::indent => indent = inner.as_str().chars().count(),
            Rule::body => {
                // Descend into the body to collect name/token/comment.
                for b in inner.into_inner() {
                    match b.as_rule() {
                        Rule::name => tokens.push(RawToken::Name(b.as_str().to_string())),
                        Rule::token => {
                            let t = b.into_inner().next().ok_or_else(|| {
                                OsdlError::Parse(ParseError::new("empty token").with_span(
                                    span_at(src, byte_start, byte_end, line_no),
                                    src,
                                ))
                            })?;
                            match t.as_rule() {
                                Rule::flag => tokens.push(RawToken::Flag(t.as_str().to_string())),
                                Rule::reference => {
                                    let (m, f) = split_reference(t.as_str());
                                    tokens.push(RawToken::Reference { model: m, field: f });
                                }
                                Rule::word => tokens.push(RawToken::Word(t.as_str().to_string())),
                                _ => {}
                            }
                        }
                        Rule::comment_part => { /* comments carry no tokens */ }
                        _ => {}
                    }
                }
            }
            Rule::newline => {}
            _ => {}
        }
    }
    Ok(ParsedLine {
        indent,
        tokens,
        line_no,
        byte_start,
        byte_end,
    })
}

fn split_reference(s: &str) -> (String, String) {
    match s.split_once('.') {
        Some((m, f)) => (m.to_string(), f.to_string()),
        None => (s.to_string(), "id".into()),
    }
}

fn span_at(_src: &str, start: usize, end: usize, line: usize) -> Span {
    Span {
        start,
        end,
        line,
        column: 1,
    }
}
