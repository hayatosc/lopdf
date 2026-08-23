use lopdf::{Document, LoadOptions, Object};

/// Append objects, returning each one's `(number, physical offset)`.
fn append_objects(pdf: &mut Vec<u8>, bodies: &[(u32, u16, String)]) -> Vec<(u32, usize)> {
    let mut offsets = Vec::new();
    for (number, generation, body) in bodies {
        offsets.push((*number, pdf.len()));
        pdf.extend_from_slice(format!("{number} {generation} obj\n{body}\nendobj\n").as_bytes());
    }
    offsets
}

/// Append a cross-reference table and trailer whose `startxref` records
/// `startxref_value` (tests pass broken values to force the fallback).
fn append_xref_trailer(pdf: &mut Vec<u8>, offsets: &[(u32, usize)], trailer: &str, startxref_value: usize) {
    pdf.extend_from_slice(b"xref\n");
    pdf.extend_from_slice(format!("0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes());
    for (_, offset) in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(format!("trailer\n{trailer}\nstartxref\n{startxref_value}\n%%EOF\n").as_bytes());
}

/// One-page document whose content stream embeds object-like decoy tokens
/// (`99 88 objx`, `1 2 objects`) that must never become markers.
fn sample_bodies() -> Vec<(u32, u16, String)> {
    let contents = b"BT /F1 12 Tf 20 100 Td (Hello World) Tj (99 88 objx) Tj (1 2 objects) Tj ET";
    vec![
        (1, 0, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (2, 0, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        (
            3,
            0,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>".to_string(),
        ),
        (
            4,
            0,
            format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                contents.len(),
                std::str::from_utf8(contents).unwrap()
            ),
        ),
    ]
}

fn new_pdf() -> Vec<u8> {
    b"%PDF-1.4\n".to_vec()
}

#[test]
fn startxref_past_eof_is_reconstructed_from_object_markers() {
    let mut pdf = new_pdf();
    let offsets = append_objects(&mut pdf, &sample_bodies());
    let broken_startxref = pdf.len() + 4096;
    append_xref_trailer(&mut pdf, &offsets, "<< /Size 5 /Root 1 0 R >>", broken_startxref);

    let document = Document::load_mem(&pdf).unwrap();
    assert_eq!(document.get_pages().len(), 1);
    assert!(document.trailer.get(b"Root").is_ok());

    assert!(document.get_object((99, 0)).is_err());

    let metadata = Document::load_metadata_mem(&pdf).unwrap();
    assert_eq!(metadata.page_count, 1);
}

#[test]
fn strict_mode_does_not_reconstruct() {
    let mut pdf = new_pdf();
    let offsets = append_objects(&mut pdf, &sample_bodies());
    let broken_startxref = pdf.len() + 4096;
    append_xref_trailer(&mut pdf, &offsets, "<< /Size 5 /Root 1 0 R >>", broken_startxref);
    let strict = LoadOptions {
        strict: true,
        ..Default::default()
    };

    assert!(Document::load_mem_with_options(&pdf, strict).is_err());
}

#[test]
fn startxref_pointing_into_a_stream_falls_back_to_reconstruction() {
    // Pointer lands mid-payload, beyond the correction window.
    let mut pdf = new_pdf();
    let offsets = append_objects(&mut pdf, &sample_bodies());
    let payload = pdf.windows(7).position(|window| window == b"stream\n").unwrap() + 7;
    append_xref_trailer(&mut pdf, &offsets, "<< /Size 5 /Root 1 0 R >>", payload + 30);

    let document = Document::load_mem(&pdf).unwrap();
    assert_eq!(document.get_pages().len(), 1);
    let Object::Stream(stream) = document.get_object((4, 0)).unwrap() else {
        panic!("reconstructed object must remain a stream");
    };
    assert!(stream.content.starts_with(b"BT"));
}

#[test]
fn reconstruction_prefers_newer_revisions_of_duplicated_objects() {
    // A second revision redefines object 2; its startxref is corrupted.
    let mut pdf = new_pdf();
    let mut offsets = append_objects(&mut pdf, &sample_bodies());
    let first_startxref = pdf.len();
    append_xref_trailer(&mut pdf, &offsets, "<< /Size 5 /Root 1 0 R >>", first_startxref);

    let revised_pages = "<< /Type /Pages /Kids [3 0 R] /Count 42 >>";
    let revision_offset = pdf.len();
    pdf.extend_from_slice(format!("2 0 obj\n{revised_pages}\nendobj\n").as_bytes());
    offsets.retain(|(number, _)| *number != 2);
    offsets.push((2, revision_offset));
    let broken_startxref = pdf.len() + 4096;
    append_xref_trailer(&mut pdf, &offsets, "<< /Size 5 /Root 1 0 R >>", broken_startxref);

    let document = Document::load_mem(&pdf).unwrap();
    let count = document
        .get_object((2, 0))
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"Count")
        .unwrap()
        .as_i64()
        .unwrap();
    assert_eq!(count, 42);
    assert_eq!(document.get_pages().len(), 1);
}

#[test]
fn reconstruction_skips_trailers_whose_root_was_not_found() {
    // A trailing bogus trailer references absent catalog object 77.
    let mut pdf = new_pdf();
    let offsets = append_objects(&mut pdf, &sample_bodies());
    let broken_startxref = pdf.len() + 4096;
    append_xref_trailer(&mut pdf, &offsets, "<< /Size 5 /Root 1 0 R >>", broken_startxref);
    pdf.extend_from_slice(b"trailer\n<< /Size 12 /Root 77 0 R >>\n%%EOF\n");

    let document = Document::load_mem(&pdf).unwrap();
    assert_eq!(document.get_pages().len(), 1);
    assert!(document.get_object((77, 0)).is_err());
}

#[test]
fn reconstruction_fails_when_no_trailer_has_a_usable_root() {
    let mut pdf = new_pdf();
    append_objects(&mut pdf, &sample_bodies());
    pdf.extend_from_slice(b"trailer\n<< /Size 5 >>\n");
    let broken_startxref = pdf.len() + 4096;
    append_xref_trailer(&mut pdf, &[], "", broken_startxref);

    assert!(Document::load_mem(&pdf).is_err());
}
