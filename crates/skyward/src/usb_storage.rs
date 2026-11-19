//! Offering a partition to a computer as a USB drive, on a kernel whose
//! mass storage function is there: the computer sees a disk rather than a
//! phone, and reads it as the file system it holds.

use anyhow::{Context, Result, ensure};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Where the kernel keeps the gadgets a recovery makes.
const GADGETS: &str = "/config/usb_gadget";

/// The gadget this recovery makes for the drive, beside the one the phone's
/// own init makes for adb and MTP.
const NAME: &str = "skyward_ums";

/// Where the kernel names the controllers the port can be.
const UDC: &str = "/sys/class/udc";

/// The kernel's own configuration, where it carries it.
const CONFIG: &str = "/proc/config.gz";

/// What the kernel says of the function that offers a partition as a disk.
const FUNCTION: &str = "CONFIG_USB_CONFIGFS_MASS_STORAGE";

/// Whether the kernel carries the mass storage function, as its own
/// configuration says: nothing is made and taken down to find out, so the
/// port the phone offers is left as it is.
pub fn supported() -> bool {
    if let Ok(mut file) = fs::File::open(CONFIG) {
        let mut text = String::new();
        let mut gz = flate2::read::GzDecoder::new(&mut file);
        if gz.read_to_string(&mut text).is_ok() {
            return text.lines().any(|line| {
                let line = line.trim();
                line.strip_prefix(FUNCTION).and_then(|rest| rest.strip_prefix('=')).is_some_and(|value| value == "y" || value == "m")
            });
        }
    }
    // A kernel that carries no configuration of its own is asked whether the
    // function's module is there.
    Path::new("/sys/module/usb_f_mass_storage").exists()
}

/// The controller the port is, by the name the kernel gives it.
fn controller() -> Result<PathBuf> {
    fs::read_dir(UDC)
        .with_context(|| format!("read {UDC}"))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .next()
        .context("the phone reports no USB controller")
}

/// Hand the partition at `device` to the computer as a disk, this gadget
/// taking the port: what the port offered before is taken away for it.
pub fn start(device: &Path) -> Result<()> {
    ensure!(supported(), "this kernel carries no mass storage function for the USB port");
    let gadget = Path::new(GADGETS).join(NAME);
    // A gadget this kernel has made once is used again rather than made
    // afresh: making one after another was taken down fails here with no
    // memory, whatever the phone has free.
    if !gadget.exists() {
        let made = || -> Result<()> {
            fs::create_dir_all(gadget.join("functions"))?;
            fs::create_dir_all(gadget.join("configs/c.1"))?;
            fs::write(gadget.join("idVendor"), "0x18d1")?;
            fs::write(gadget.join("idProduct"), "0x4ee2")?;
            let lun = gadget.join("functions/mass_storage.0");
            fs::create_dir(&lun)?;
            fs::write(lun.join("lun.0/removable"), "1")?;
            std::os::unix::fs::symlink(&lun, gadget.join("configs/c.1/mass_storage.0"))?;
            Ok(())
        };
        made().with_context(|| format!("make the gadget in {}", gadget.display()))?;
    }
    // Read-only: what the computer is offered is copied out of the phone, not
    // written into a file the recovery may be reading at the same time.
    let lun = gadget.join("functions/mass_storage.0/lun.0");
    fs::write(lun.join("ro"), "1")?;
    fs::write(lun.join("file"), device.display().to_string()).context("give the drive the file")?;
    let udc = controller()?;
    let name = udc.file_name().context("the controller has no name")?.to_string_lossy().into_owned();
    fs::write(gadget.join("UDC"), name).with_context(|| format!("bind the gadget to {}", udc.display()))?;
    Ok(())
}

/// Take the drive away and let the port go, so that what the phone offers
/// can be set up again.
pub fn stop() -> Result<()> {
    let gadget = Path::new(GADGETS).join(NAME);
    if !gadget.exists() {
        return Ok(());
    }
    // The port is let go and the partition released; the gadget itself stays,
    // as it is used again the next time rather than made afresh. Both are
    // read back: a port left bound keeps the computer's disk and takes adb
    // away with it, which nothing tells from the write having been taken.
    let udc = gadget.join("UDC");
    let let_go = fs::write(&udc, "");
    let held = fs::read_to_string(&udc).unwrap_or_default();
    let file = gadget.join("functions/mass_storage.0/lun.0/file");
    let released = fs::write(&file, "");
    let bound = fs::read_to_string(&file).unwrap_or_default();
    // What is left behind is said, not refused: the caller gives the port
    // back either way, so that adb is never lost with a disk.
    match (let_go, released) {
        (Ok(()), Ok(())) if held.trim().is_empty() && bound.trim().is_empty() => Ok(()),
        _ => Err(anyhow::anyhow!("the gadget kept: port {:?}, file {:?}", held.trim(), bound.trim())),
    }
}
