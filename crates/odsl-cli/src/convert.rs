//! `odsl convert` — transpile between ODSL and Prisma / Drizzle schemas.

use crate::project::io_err;
use odsl_codegen_drizzle::{drizzle_to_odsl, render_drizzle};
use odsl_codegen_prisma::{prisma_to_odsl, render_prisma};
use odsl_core::Target;
use odsl_core::errors::OdslError;
use odsl_parser::parse;

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConvertDirection {
    ToPrisma,
    FromPrisma,
    ToDrizzle,
    FromDrizzle,
}

pub fn cmd_convert(
    direction: ConvertDirection,
    input: &std::path::Path,
    out: &std::path::Path,
) -> Result<(), OdslError> {
    let src = std::fs::read_to_string(input)
        .map_err(|e| io_err(format!("reading {}: {e}", input.display())))?;
    let body = match direction {
        ConvertDirection::ToPrisma => {
            // Validate the ODSL first so we don't emit a prisma doc from a
            // broken schema.
            let ast = parse(&src)?;
            odsl_core::Validator::validate(&ast, Some(Target::SeaOrmSqlite))?;
            render_prisma(&ast)
        }
        ConvertDirection::FromPrisma => {
            // Parse + serialize through the canonical ODSL formatter; any
            // import error surfaces here.
            prisma_to_odsl(&src).map_err(|e| io_err(format!("converting prisma: {e}")))?
        }
        ConvertDirection::ToDrizzle => {
            let ast = parse(&src)?;
            odsl_core::Validator::validate(&ast, Some(Target::SeaOrmSqlite))?;
            render_drizzle(&ast)
        }
        ConvertDirection::FromDrizzle => {
            drizzle_to_odsl(&src).map_err(|e| io_err(format!("converting drizzle: {e}")))?
        }
    };
    std::fs::write(out, &body).map_err(|e| io_err(format!("writing {}: {e}", out.display())))?;
    println!("wrote {} (direction: {direction:?})", out.display());
    Ok(())
}
