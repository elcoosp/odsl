fn rt(label: &str, src: &str) {
    let a = osdl_parser::parse(src).expect("parse1");
    let formatted = osdl_core::formatter::format_ast(&a);
    let b = osdl_parser::parse(&formatted).expect("parse2");
    assert_eq!(
        a.views.len(),
        b.views.len(),
        "view count mismatch for {label}"
    );
    for (va, vb) in a.views.iter().zip(b.views.iter()) {
        assert_eq!(va.name, vb.name, "name {label}");
        assert_eq!(va.materialized, vb.materialized, "materialized {label}");
        assert_eq!(va.fields, vb.fields, "fields {label}");
        assert_eq!(va.query, vb.query, "query {label}");
    }
    println!("{label} round-trip OK\n--- formatted ---\n{formatted}\n---");
}

fn main() {
    rt(
        "full",
        "view UserSummary id uuid, email string -materialized = SELECT u.id, u.email FROM users u\n",
    );
    rt(
        "multiline",
        "view ActiveUsers id uuid, email string -materialized =\n  SELECT u.id, u.email\n  FROM users u\n  WHERE u.active = true\n",
    );
    rt(
        "bare",
        "view RecentPosts = SELECT p.id, p.title FROM posts p ORDER BY p.created_at DESC\n",
    );
}
