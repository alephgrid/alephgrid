//! 1.10: the frontend's protocol types are generated from the Rust types and must match.

#[test]
fn protocol_bindings_are_up_to_date() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/protocol.ts");
    let generated = alephgrid_server::protocol::bindings();
    if std::env::var("UPDATE_BINDINGS").is_ok() {
        std::fs::write(&path, &generated).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == generated,
        "web/src/protocol.ts is out of date with the Rust types; run \
         `UPDATE_BINDINGS=1 cargo test -p alephgrid-server --test protocol` and commit the result"
    );
}

#[test]
fn every_op_has_a_result_type() {
    let ops = alephgrid_server::protocol::op_names();
    let results: Vec<String> = alephgrid_server::protocol::RESULTS.iter().map(|(o, _)| o.to_string()).collect();
    assert!(!ops.is_empty());
    assert_eq!(ops, results, "protocol::RESULTS must list every Op, in order");
}

/// A field the TypeScript types mark optional (`x?: T`) must be omitted when empty,
/// never sent as `null`, or the frontend reads `null` where it expects a value or
/// `undefined`. Response types only; request types may accept either.
#[test]
fn optional_fields_are_omitted_not_null() {
    let src_files = ["../alephgrid-core/src/model.rs", "../alephgrid-core/src/import.rs", "../alephgrid-core/src/workbook.rs",
        "../alephgrid-core/src/sheets.rs", "../alephgrid-core/src/sqlpolicy.rs", "src/state.rs", "src/http.rs", "src/ws.rs"];
    let mut problems = Vec::new();
    for f in src_files {
        let text = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(f)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            // Requests (Op variants with defaults, optional_fields containers) may omit fields.
            if !line.contains("#[ts(optional") || line.contains("as = \"Option<") || line.contains("optional_fields") {
                continue;
            }
            // Look at the field's attribute block (lines above until the field line).
            let mut j = i;
            while j > 0 && lines[j - 1].trim_start().starts_with("#[") {
                j -= 1;
            }
            let mut k = i;
            while k + 1 < lines.len() && lines[k].trim_start().starts_with("#[") {
                k += 1;
            }
            let attrs = lines[j..k].join("\n");
            let field = lines[k].trim();
            let input_only = field.contains("ImportRequest") || attrs.contains("as = \"Option<");
            if !attrs.contains("skip_serializing_if") && !input_only && !field.starts_with("pub table") && !field.starts_with("pub job") {
                problems.push(format!("{f}:{}: {field}", k + 1));
            }
        }
    }
    assert!(problems.is_empty(), "optional fields sent as null:\n{}", problems.join("\n"));
}

/// Op parameters share the request frame with its `id`, so no op may have a field `id`.
#[test]
fn no_op_parameter_is_named_id() {
    let decl = <alephgrid_server::ws::Op as ts_rs::TS>::decl(&ts_rs::Config::new());
    assert!(!decl.contains(" id:") && !decl.contains(" id?:"), "{decl}");
}
