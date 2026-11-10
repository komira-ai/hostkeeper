use hostkeeper_mdm_core::package::{Layout, UNITS};

use super::deb_files;

#[test]
fn mdms_deb_holds_its_programs_in_usr_lib_mdm_and_its_eight_units() {
    // Catches: a tool or the program left out or put elsewhere, a hook wrapper added again, a unit left
    // out (only the run units, say), or a unit still starting a program through the archive's
    // current.
    let tools = vec![
        ("caddy".to_owned(), b"C".to_vec()),
        ("lego".to_owned(), b"L".to_vec()),
    ];
    let files = deb_files(b"M".to_vec(), tools);
    let mut want: Vec<(String, u32)> = vec![
        ("/usr/lib/mdm/caddy".to_owned(), 0o755),
        ("/usr/lib/mdm/lego".to_owned(), 0o755),
        ("/usr/lib/mdm/mdm".to_owned(), 0o755),
    ];
    want.extend(
        UNITS
            .iter()
            .map(|u| (format!("/usr/lib/systemd/system/{}", u.name), 0o644)),
    );
    want.sort();
    let got: Vec<(String, u32)> = files.iter().map(|f| (f.path.clone(), f.mode)).collect();
    assert_eq!(got, want);
    let data = |path: &str| {
        files
            .iter()
            .find(|f| f.path == path)
            .expect(path)
            .data
            .clone()
    };
    assert_eq!(data("/usr/lib/mdm/mdm"), b"M");
    assert_eq!(data("/usr/lib/mdm/caddy"), b"C");
    for unit in &UNITS {
        assert_eq!(
            data(&format!("/usr/lib/systemd/system/{}", unit.name)),
            Layout::Deb.unit_text(unit).into_bytes()
        );
    }
}
