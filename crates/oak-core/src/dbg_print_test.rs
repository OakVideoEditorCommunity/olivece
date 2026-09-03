#[test]
fn dbg_print() {
    let mut sp = crate::subtitleparams::SubtitleParams::new();
    sp.add_subtitle(1, 1, 2, 1, "a < b & \"c\" > d").unwrap();
    let xml = sp.save_xml().unwrap();
    println!("XML_OUTPUT_START>>>{}<<<XML_OUTPUT_END", xml);
}
