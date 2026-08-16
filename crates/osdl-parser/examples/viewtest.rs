fn tryit(label: &str, src: &str) {
    match osdl_parser::parse(src) {
        Ok(a) => {
            for v in &a.views {
                println!(
                    "{} OK view name={} materialized={} fields={:?} query={:?}",
                    label, v.name, v.materialized, v.fields, v.query
                );
            }
        }
        Err(e) => println!("{} ERR: {}", label, e),
    }
}

fn main() {
    tryit(
        "full",
        "view UserSummary id uuid, email string -materialized = SELECT u.id, u.email FROM users u\n",
    );
    tryit(
        "multiline",
        "view ActiveUsers id uuid, email string -materialized =\n  SELECT u.id, u.email\n  FROM users u\n  WHERE u.active = true\n",
    );
    tryit(
        "bare",
        "view RecentPosts = SELECT p.id, p.title FROM posts p ORDER BY p.created_at DESC\n",
    );
}
