use super::*;

/// Render a Jupyter `.ipynb` (nbformat 4) as readable cells instead of raw
/// JSON. Returns `None` when the bytes don't parse as a notebook so the caller
/// falls back to the plain-text reader. Notebooks render as cells with outputs
/// and pair with `notebook_edit` — cell ids/indices are shown so the model can
/// target a cell. Binary outputs (images, etc.) become a placeholder so a
/// base64 PNG can't flood the context.
pub(super) fn render_notebook(display_path: &str, text: &str) -> Option<String> {
    const MAX_OUTPUT_CHARS: usize = 2000;
    let nb: Value = serde_json::from_str(text).ok()?;
    let cells = nb.get("cells")?.as_array()?;
    let mut out = format!(
        "<system-reminder>`{}` is a Jupyter notebook ({} cells), rendered as cells below (not raw JSON). \
         Edit a cell with `notebook_edit`.</system-reminder>\n",
        display_path,
        cells.len()
    );
    for (i, cell) in cells.iter().enumerate() {
        let cell_type = cell
            .get("cell_type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let id_note = match cell.get("id").and_then(|v| v.as_str()) {
            Some(id) => format!(" id={id}"),
            None => String::new(),
        };
        out.push_str(&format!("\n[cell {i}{id_note}] {cell_type}\n"));
        let source = join_nb_text(cell.get("source"));
        if source.trim().is_empty() {
            out.push_str("(empty)\n");
        } else {
            out.push_str(&source);
            if !source.ends_with('\n') {
                out.push('\n');
            }
        }
        if cell_type == "code" {
            if let Some(outputs) = cell.get("outputs").and_then(|v| v.as_array()) {
                if let Some(rendered) = render_nb_outputs(outputs, MAX_OUTPUT_CHARS) {
                    out.push_str("--- output ---\n");
                    out.push_str(&rendered);
                    out.push('\n');
                }
            }
        }
    }
    Some(out)
}

/// nbformat stores text fields (`source`, stream `text`, `text/plain`) as either
/// a string or an array of line-strings; join either into one string.
pub(super) fn join_nb_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items.iter().filter_map(|x| x.as_str()).collect(),
        _ => String::new(),
    }
}

/// Render a code cell's `outputs`: text streams / `text/plain` results / errors
/// are shown (truncated to `max_chars`); rich or binary mimes become a
/// `[<mime> output omitted]` placeholder. `None` when there is nothing textual.
pub(super) fn render_nb_outputs(outputs: &[Value], max_chars: usize) -> Option<String> {
    let mut buf = String::new();
    for o in outputs {
        let piece = match o.get("output_type").and_then(|v| v.as_str()).unwrap_or("") {
            "stream" => join_nb_text(o.get("text")),
            "execute_result" | "display_data" => match o.get("data") {
                Some(Value::Object(data)) => {
                    if let Some(t) = data.get("text/plain") {
                        join_nb_text(Some(t))
                    } else {
                        let mimes: Vec<&str> = data.keys().map(|k| k.as_str()).collect();
                        format!("[{} output omitted]", mimes.join(", "))
                    }
                }
                _ => String::new(),
            },
            "error" => {
                let ename = o.get("ename").and_then(|v| v.as_str()).unwrap_or("Error");
                let evalue = o.get("evalue").and_then(|v| v.as_str()).unwrap_or("");
                format!("{ename}: {evalue}")
            }
            _ => String::new(),
        };
        if piece.trim().is_empty() {
            continue;
        }
        if !buf.is_empty() {
            buf.push('\n');
        }
        buf.push_str(piece.trim_end_matches('\n'));
    }
    if buf.is_empty() {
        return None;
    }
    if buf.chars().count() > max_chars {
        let truncated: String = buf.chars().take(max_chars).collect();
        buf = format!("{truncated}… [output truncated]");
    }
    Some(buf)
}
