//! Stable renderers for the report rows.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Table,
    Json,
    Csv,
    Markdown,
    JsonLines,
}

pub fn render(
    rows: &[Value],
    columns: &[&str],
    format: OutputFormat,
) -> Result<String, crate::VivariumError> {
    match format {
        OutputFormat::Json => serde_json::to_string_pretty(rows)
            .map_err(|error| crate::VivariumError::Other(format!("report JSON: {error}"))),
        OutputFormat::JsonLines => json_lines(rows),
        OutputFormat::Csv => Ok(csv(rows, columns)),
        OutputFormat::Markdown => Ok(markdown(rows, columns)),
        OutputFormat::Table => Ok(table(rows, columns)),
    }
}

fn json_lines(rows: &[Value]) -> Result<String, crate::VivariumError> {
    let mut out = String::new();
    for row in rows {
        let line = serde_json::to_string(row)
            .map_err(|error| crate::VivariumError::Other(format!("report JSONL: {error}")))?;
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

fn csv(rows: &[Value], columns: &[&str]) -> String {
    let mut out = columns.join(",");
    out.push('\n');
    for row in rows {
        let cells = columns.iter().map(|column| csv_cell(&cell(row, column)));
        out.push_str(&cells.collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    out
}

fn csv_cell(value: &str) -> String {
    if value
        .chars()
        .any(|ch| matches!(ch, ',' | '"' | '\n' | '\r'))
    {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn markdown(rows: &[Value], columns: &[&str]) -> String {
    let mut out = String::new();
    let header = columns
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    line(&mut out, &header);
    line(
        &mut out,
        &columns.iter().map(|_| "---".to_owned()).collect::<Vec<_>>(),
    );
    for row in rows {
        let values = columns
            .iter()
            .map(|column| cell(row, column).replace('|', "\\|"))
            .collect::<Vec<_>>();
        line(&mut out, &values);
    }
    out
}

fn line(out: &mut String, cells: &[String]) {
    out.push('|');
    for cell in cells {
        out.push(' ');
        out.push_str(cell);
        out.push_str(" |");
    }
    out.push('\n');
}

fn table(rows: &[Value], columns: &[&str]) -> String {
    let rendered: Vec<Vec<String>> = rows
        .iter()
        .map(|row| columns.iter().map(|column| cell(row, column)).collect())
        .collect();
    let widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            rendered
                .iter()
                .map(|row| row[index].chars().count())
                .fold(column.chars().count(), usize::max)
                .min(48)
        })
        .collect();
    let mut out = String::new();
    write_row(
        &mut out,
        &columns
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>(),
        &widths,
    );
    write_row(
        &mut out,
        &rendered.first().map_or_else(
            || Vec::new(),
            |_| columns.iter().map(|_| "-".to_owned()).collect(),
        ),
        &widths,
    );
    for row in rendered {
        write_row(&mut out, &row, &widths);
    }
    out
}

fn write_row(out: &mut String, row: &[String], widths: &[usize]) {
    for (index, (cell, width)) in row.iter().zip(widths).enumerate() {
        if index > 0 {
            out.push_str("  ");
        }
        let shown: String = cell.chars().take(*width).collect();
        out.push_str(&shown);
        for _ in shown.chars().count()..*width {
            out.push(' ');
        }
    }
    out.push('\n');
}

fn cell(row: &Value, column: &str) -> String {
    match row.get(column) {
        None | Some(Value::Null) => "unavailable".into(),
        Some(Value::String(value)) => value.clone(),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| value.as_str().unwrap_or("").to_owned())
            .collect::<Vec<_>>()
            .join(", "),
        Some(value) => value.to_string(),
    }
}
