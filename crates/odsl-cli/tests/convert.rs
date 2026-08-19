//! Integration tests for `odsl convert` (ODSL <-> Prisma Schema Language).

use std::process::Command;

fn odsl() -> Command {
    Command::new(env!("CARGO_BIN_EXE_odsl"))
}

#[test]
fn convert_to_prisma_exports_models_and_relations() {
    let dir = std::env::temp_dir().join(format!("odsl-convert-it-{}-{}", std::process::id(), "to"));
    let _ = std::fs::create_dir_all(&dir);

    let schema = dir.join("schema.odsl");
    let _ = std::fs::write(
        &schema,
        "User\n  id uuid -pk\n  email string -uniq\n  age int -null\n\nPost\n  id uuid -pk\n  author User.id\n",
    );
    let out = dir.join("schema.prisma");

    let status = odsl()
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
fn convert_from_prisma_round_trips_back_to_odsl() {
    let dir =
        std::env::temp_dir().join(format!("odsl-convert-it-{}-{}", std::process::id(), "from"));
    let _ = std::fs::create_dir_all(&dir);

    let prisma = dir.join("schema.prisma");
    let _ = std::fs::write(
        &prisma,
        "model User {\n  id String @id @default(uuid())\n  email String @unique\n  age Int?\n  name String\n}\n\nmodel Post {\n  id String @id @default(uuid())\n  author User @relation(fields: [author], references: [id])\n}\n",
    );
    let out = dir.join("back.odsl");

    let status = odsl()
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

    let odsl = std::fs::read_to_string(&out).expect("read odsl output");
    assert!(odsl.contains("User"));
    assert!(odsl.contains("Post"));
    assert!(odsl.contains("author User.id"));
    assert!(odsl.contains("-pk"));
    assert!(odsl.contains("-uniq"));
    assert!(odsl.contains("-null"));
}
