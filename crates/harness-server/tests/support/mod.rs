use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Writes `contents` to `path` as a mode-0755 executable without this process
/// ever holding a writable descriptor for it.
///
/// Tests run on parallel threads. If this process opened the script itself, a
/// child forked by another test thread could inherit the write descriptor
/// until its `exec` closes it, and executing the script during that window
/// fails with `ETXTBSY` ("Text file busy"). A short-lived `sh` writes the file
/// instead, so no write descriptor outlives the call.
pub fn write_executable(path: &Path, contents: impl AsRef<[u8]>) {
    let mut child = Command::new("sh")
        .args(["-c", r#"cat > "$1" && chmod 755 "$1""#, "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn sh to write executable");
    child
        .stdin
        .take()
        .expect("executable writer stdin")
        .write_all(contents.as_ref())
        .expect("write executable contents");
    let status = child.wait().expect("wait for executable writer");
    assert!(
        status.success(),
        "writing {} failed: {status}",
        path.display()
    );
}
