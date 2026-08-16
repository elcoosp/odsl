//! Integration tests for `osdl convert` (OSDL <-> Prisma Schema Language).

use std::process::Command;

fn osdl() -> Command {
    Command::new(env!("CARGO_BIN_EXE_osdl"))
}

#[test]
fn convert_to_prisma_exports_models_and_relations() {
    let dir = std::env::temp_dir().join(format!("osdl-convert-it-{}-{}", std::process::id(), "to"));
    let _ = std::fs::create_dir_all(&dir);

    let schema = dir.join("schema.osdl");
    let _ = std::fs::write(
        &schema,
        "User\n  id uuid -pk\n  email string -uniq\n  age int -null\n\nPost\n  id uuid -pk\n  author User.id\n",
    );
    let out = dir.join("schema.prisma");

    let status = osdl()
        .arg("convert")
        .arg("--direction")
        .arg("to-prisma")
        .arg(&schema)
        .arg(&out)
        .output()
        .expect("run convert to-prisma");
    assert!(
        status.status.success(),
        "convert to-prisma failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    let prisma = std::fs::read_to_string(&out).expect("read prisma output");
    assert!(prisma.contains("model User {"));
    assert!(prisma.contains("model Post {"));
    assert!(prisma.contains("id String @id @default(uuid())"));
    assert!(prisma.contains("email String @unique"));
    assert!(prisma.contains("age Int?"));
    assert!(prisma.contains("@relation(fields: [author], references: [id])"));
}

#[test]
fn convert_from_prisma_round_trips_back_to_osdl() {
    let dir =
        std::env::temp_dir().join(format!("osdl-convert-it-{}-{}", std::process::id(), "from"));
    let _ = std::fs::create_dir_all(&dir);

    let prisma = dir.join("schema.prisma");
    let _ = std::fs::write(
        &prisma,
        "model User {\n  id String @id @default(uuid())\n  email String @unique\n  age Int?\n  name String\n}\n\nmodel Post {\n  id String @id @default(uuid())\n  author User @relation(fields: [author], references: [id])\n}\n",
    );
    let out = dir.join("back.osdl");

    let status = osdl()
        .arg("convert")
        .arg("--direction")
        .arg("from-prisma")
        .arg(&prisma)
        .arg(&out)
        .output()
        .expect("run convert from-prisma");
    assert!(
        status.status.success(),
        "convert from-prisma failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    let osdl = std::fs::read_to_string(&out).expect("read osdl output");
    assert!(osdl.contains("User"));
    assert!(osdl.contains("Post"));
    assert!(osdl.contains("author User.id"));
    assert!(osdl.contains("-pk"));
    assert!(osdl.contains("-uniq"));
    assert!(osdl.contains("-null"));
}
