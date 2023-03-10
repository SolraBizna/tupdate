use std::{
    cell::RefCell,
    env::current_exe,
    fs::File,
    io::{Read, BufRead, BufReader, ErrorKind, Write},
    process::ExitCode,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Mutex,
    sync::atomic::{AtomicUsize, Ordering as AtomicOrdering},
    time::Instant,
};

use clap::Parser;
use rayon::prelude::*;
use url::Url;
use wax::Glob;

mod gui;
use gui::*;

mod update_finder;
use update_finder::find_updates;

mod patience;
use patience::Patience;

const CONFIG_FILE_PATH: &str = "tupdate.conf";

fn is_fishy_path(target: &str) -> bool {
    target.starts_with(".") || target.starts_with("/") || target.starts_with("\\") || target.find("/.").is_some() || target.find("\\.").is_some()
}

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Invocation {
    /// Which GUI to use. Use `--gui help` for more information.
    #[arg(short, long)]
    gui: Option<String>,
    /// Whether to output extra status information about what we're doing and
    /// why.
    #[arg(short, long)]
    verbose: bool,
    #[arg(short, long)]
    /// Pause and wait for a response after every dialog, if supported by the
    /// selected GUI. Default depends on the GUI and the platform.
    #[arg(short, long)]
    pause: Option<bool>,
    target_url: Option<Url>,
}

#[derive(Debug)]
struct Cat {
    src_url: Url,
    dst_path: PathBuf,
    checksum: [u8; 32],
    size: u64,
    needs_download: bool,
}

impl Cat {
    fn try_parse<'a>(bytes: &'a [u8], base_url: &Url, base_path: &Path) -> Result<(Cat, &'a [u8]), ()> {
        let newline = bytes.iter().position(|x| *x == b'\n').ok_or(())?;
        if newline == 0 { return Err(()) }
        if newline > bytes.len() - 42 { return Err(()) }
        let file_path = &bytes[..newline];
        let file_path = std::str::from_utf8(file_path).map_err(|_| ())?;
        let checksum = &bytes[newline+1 .. newline+33];
        let size = u64::from_be_bytes(bytes[newline+33 .. newline+41].try_into().unwrap());
        let xt = u16::from_be_bytes(bytes[newline+41 .. newline+43].try_into().unwrap());
        let next = newline + 43 + xt as usize;
        if next > bytes.len() { return Err(()) }
        if is_fishy_path(file_path) { return Err(()) }
        let src_url = base_url.join(file_path).map_err(|_| ())?;
        Ok((Cat {
            src_url,
            dst_path: base_path.join(file_path),
            checksum: checksum.try_into().unwrap(),
            size,
            needs_download: false,
        }, &bytes[next..]))
    }
}

fn try_load_url_from_file(gui: &Rc<RefCell<dyn Gui>>, verbose: bool, path: &Path) -> Option<Url> {
    if verbose {
        gui.borrow_mut().verbose(&format!("Looking for update URL in: {:?}", path));
    }
    let f = match File::open(path) {
        Ok(x) => x,
        Err(x) => {
            if verbose {
                gui.borrow_mut().verbose(&format!("  {}", x));
            }
            return None
        },
    };
    let f = BufReader::new(f);
    for line in f.lines() {
        let line = line.expect("IO error while reading tupdate.conf!");
        if line.starts_with("URL=") {
            let slab = &line[4..];
            let url = match Url::parse(slab) {
                Ok(x) => x,
                Err(_) => {
                    if verbose {
                        gui.borrow_mut().verbose(&format!("  File exists, but its URL= line does not contain a valid URL"));
                    }
                    return None
                },
            };
            if verbose {
                gui.borrow_mut().verbose(&format!("  {}", url));
            }
            return Some(url);
        }
    }
    if verbose {
        gui.borrow_mut().verbose(&format!("  File exists, but has no URL= line"));
    }
    None
}

fn find_target_url(gui: &Rc<RefCell<dyn Gui>>, verbose: bool, mut target_url: Option<Url>) -> Result<Url, ()> {
    if target_url == None {
        // Look next to the executable first.
        if let Ok(mut exe_path) = current_exe() {
            exe_path.pop();
            exe_path.push(CONFIG_FILE_PATH);
            target_url = try_load_url_from_file(&gui, verbose, &exe_path);
        }
    }
    if target_url == None {
        // Look in the working directory.
        target_url = try_load_url_from_file(&gui, verbose, Path::new(CONFIG_FILE_PATH));
    }
    let target_url = match target_url {
        None => {
            gui.borrow_mut().do_error("No URL specified", &format!("Couldn't determine what URL to update from. Either pass one on the command line, or create a {:?}.", CONFIG_FILE_PATH));
            return Err(());
        },
        Some(x) => x,
    };
    match target_url.scheme() {
        "http" | "https" => (), // okay
        x => {
            eprintln!("{:?} is not a supported URL scheme. Only http and https are supported.", x);
            return Err(());
        },
    }
    return Ok(target_url)
}

async fn determine_tasks(gui: &Rc<RefCell<dyn Gui>>, verbose: bool, client: &mut reqwest::Client, target_url: &Url) -> Result<(Vec<Cat>, Vec<PathBuf>), ()> {
    gui.borrow_mut().set_progress("Downloading update index...", "", None);
    let body = match client.get(target_url.clone()).send().await {
        Ok(x) if x.status() == 200 => x.bytes().await.unwrap(),
        Ok(x) => {
            gui.borrow_mut().do_error("Download failed", &format!("Error \"{}\" while trying to download the update index.", x.status()));
            return Err(());
        },
        Err(x) => {
            gui.borrow_mut().do_error("Download failed", &format!("Couldn't download the update index. The error was:\n{}", x));
            return Err(());
        },
    };
    gui.borrow_mut().set_progress("Determining files to update...", "", None);
    let (installs, deletes) = match find_updates(gui.clone(), verbose, &body[..], target_url.clone()) {
        Ok(x) => x,
        Err(_) => return Err(()),
    };
    let mut all_deletions = vec![];
    for (base, globs) in deletes.into_iter() {
        for glob in globs.into_iter() {
            let glob = Glob::new(&glob).unwrap(); // already checked for validity by find_updates
            for path in glob.walk(&base) {
                let path = match path {
                    Ok(x) => x,
                    Err(x) => {
                        gui.borrow_mut().do_error("Error checking files to delete", &format!("An error occurred while trying to look through files we might need to delete. The error was:\n{}", x));
                        return Err(())
                    },
                };
                all_deletions.push(path);
            }
        }
    }
    all_deletions.sort_by(|a,b| {
        a.path().cmp(&b.path())
    });
    all_deletions.dedup_by(|a,b| { a.path() == b.path() });
    let mut all_cats = vec![];
    let mut patience = Patience::new();
    for (n, (basedir, caturl)) in installs.iter().enumerate() {
        if patience.have_been_patient() {
            gui.borrow_mut().set_progress("Downloading update catalogs...", &format!("{}/{} {}", n+1, installs.len(), caturl), Some(n as f32 / installs.len() as f32));
        }
        let body = match client.get(caturl.clone()).send().await {
            Ok(x) if x.status() == 200 => x.bytes().await.unwrap(),
            Ok(x) => {
                gui.borrow_mut().do_error("Download failed", &format!("Error \"{}\" while trying to download an update catalog.", x.status()));
                return Err(());
            },
            Err(x) => {
                gui.borrow_mut().do_error("Download failed", &format!("Couldn't download an update catalog. The error was:\n{}", x));
                return Err(());
            },
        };
        if body.len() == 0 {
            if verbose {
                gui.borrow_mut().verbose(&format!("{}: empty cat body", caturl));
            }
            gui.borrow_mut().do_error("Missing catalog", &format!("A catalog file was completely empty. This may indicate that the update server is being updated. Try again in a few minutes.\nThe corrupted catalog is: {}", caturl));
            return Err(());
        }
        if &body[..5] != b"\xFFTCat" {
            if verbose {
                gui.borrow_mut().verbose(&format!("{}: invalid cat header", caturl));
            }
            gui.borrow_mut().do_error("Invalid catalog", &format!("A catalog file was invalid. This is a problem with the update server. Try again in a few minutes.\nThe corrupted catalog is: {}", caturl));
            return Err(());
        }
        let checksum = &body[5..37];
        let uncompressed_size = u32::from_be_bytes(body[37..41].try_into().unwrap()) as usize;
        let mut uncompressed = Vec::with_capacity(uncompressed_size as usize);
        let mut reader = flate2::read::ZlibDecoder::new(&body[41..]);
        if reader.read_to_end(&mut uncompressed).is_err() || uncompressed.len() != uncompressed_size || lsx::sha256::hash(&uncompressed) != checksum {
            if verbose {
                gui.borrow_mut().verbose(&format!("{}: failed decompression", caturl));
            }
            gui.borrow_mut().do_error("Invalid catalog", &format!("A catalog file was invalid. This is a problem with the update server. Try again in a few minutes.\nThe corrupted catalog is: {}", caturl));
            return Err(());
        }
        let mut next: &[u8] = &uncompressed;
        while next.len() > 0 {
            let (cat, rem) = match Cat::try_parse(next, &caturl, basedir) {
                Ok(x) => x,
                Err(_) => {
                    if verbose {
                        gui.borrow_mut().verbose(&format!("{}: failed cat parsing", caturl));
                    }
                    gui.borrow_mut().do_error("Invalid catalog", &format!("A catalog file was invalid. This is a problem with the update server. Try again in a few minutes.\nThe corrupted catalog is: {}", caturl));
                    return Err(());
                },
            };
            all_cats.push(cat);
            next = rem;
        }
    }
    Ok((all_cats, all_deletions.into_iter().map(|x| x.into_path()).collect()))
}

fn find_cat_statuses(gui: &Rc<RefCell<dyn Gui>>, verbose: bool, all_cats: &mut Vec<Cat>) -> Result<(),()> {
    gui.borrow_mut().set_progress("Examining local files...", "", Some(0.0));
    let gui = &mut *gui.borrow_mut();
    let gui = Mutex::new(gui);
    let n = AtomicUsize::new(0);
    let num_cats = all_cats.len();
    all_cats.par_iter_mut().for_each(|cat| {
        let progn = n.fetch_add(1, AtomicOrdering::SeqCst);
        let testn = n.load(AtomicOrdering::SeqCst);
        if testn == progn {
            gui.lock().unwrap().set_progress("Examining local files...", "", Some(testn as f32 / num_cats as f32));
        }
        let meta = match std::fs::metadata(&cat.dst_path) {
            Ok(x) => x,
            Err(x) => {
                if x.kind() != ErrorKind::NotFound && verbose {
                    gui.lock().unwrap()
                    .verbose(&format!("{:?}: error getting metadata: {}", &cat.dst_path, x));
                }
                cat.needs_download = true;
                return;
            },
        };
        if meta.len() != cat.size {
            if verbose {
                gui.lock().unwrap()
                .verbose(&format!("{:?}: size does not match", &cat.dst_path));
            }
            cat.needs_download = true;
            return;
        }
        let mut f = match File::open(&cat.dst_path) {
            Ok(x) => x,
            Err(x) => {
                if x.kind() != ErrorKind::NotFound && verbose {
                    gui.lock().unwrap()
                    .verbose(&format!("{:?}: error opening file: {}", &cat.dst_path, x));
                }
                cat.needs_download = true;
                return;
            },
        };
        let mut hasher = lsx::sha256::BufSha256::new();
        let mut buf = [0u8; 32768];
        loop {
            let red = match f.read(&mut buf[..]) {
                Ok(0) => break,
                Ok(x) => x,
                Err(x) => {
                    if verbose {
                        gui.lock().unwrap()
                        .verbose(&format!("{:?}: error while reading: {}", &cat.dst_path, x));
                    }
                    cat.needs_download = true;
                    return;
                },
            };
            hasher.update(&buf[..red]);
        }
        let checksum = hasher.finish(&[]);
        if checksum != cat.checksum {
            if verbose {
                gui.lock().unwrap()
                .verbose(&format!("{:?}: checksum does not match", &cat.dst_path));
            }
            cat.needs_download = true;
        }
    });
    Ok(())
}

fn trim_deletions(gui: &Rc<RefCell<dyn Gui>>, verbose: bool, all_cats: &mut Vec<Cat>, all_deletions: &mut Vec<PathBuf>) {
    for cat in all_cats.iter() {
        let mut pat = Some(cat.dst_path.as_path());
        while let Some(dis) = pat {
            if let Ok(x) = all_deletions.binary_search_by(|el| {
                el.as_path().cmp(dis)
            }) {
                all_deletions.remove(x);
            }
            pat = dis.parent();
        }
    }
    if verbose {
        let mut gui = gui.borrow_mut();
        for deletion in all_deletions.iter() {
            gui.verbose(&format!("will delete: {:?}", deletion));
        }
        for cat in all_cats.iter() {
            if cat.needs_download {
                gui.verbose(&format!("will download: {:?} <- {}", cat.dst_path, cat.src_url));
            }
        }
    }
}

const SECONDS_PER_DAY: f64 = 86400.0;

fn calc_rate_and_eta(start_time: Instant, now: Instant, got_so_far: u64, total_to_get: u64) -> String {
    if start_time > now { return "?????????".to_string() }
    let time_so_far = (now - start_time).as_secs_f64();
    if time_so_far < 1.0 || got_so_far >= total_to_get { return "...".to_string() }
    let bytes_per_second = got_so_far as f64 / time_so_far;
    let remaining_seconds = (total_to_get - got_so_far) as f64 / bytes_per_second;
    let eta = if remaining_seconds >= 100000.0 {
        let num_days = (remaining_seconds / SECONDS_PER_DAY).floor() as u64;
        if num_days == 1 { format!("over a day left") }
        else { format!("over {} days left", num_days) }
    } else {
        let seconds = remaining_seconds.floor() as u32;
        format!("{}:{:02}:{:02} left", seconds / 60 / 60, (seconds / 60) % 60, seconds % 60)
    };
    let rate = if bytes_per_second > 1000000000.0 { format!("Wow!") }
    else if bytes_per_second > 800000.0 { format!("{:.1}MB/s", bytes_per_second / 1000000.0) }
    else if bytes_per_second > 800.0 { format!("{:.1}kB/s", bytes_per_second / 1000.0) }
    else { format!("{:.1}B/s", bytes_per_second) };
    format!("{}, {}", rate, eta)
}

async fn perform_downloads(gui: &Rc<RefCell<dyn Gui>>, verbose: bool, client: &mut reqwest::Client, all_cats: Vec<Cat>) -> Result<(),()> {
    let total_cat_bytes = all_cats.iter().fold(0, |a,x| a + if x.needs_download { x.size } else { 0 });
    let mut total_recvd_bytes = 0;
    let start_time = Instant::now();
    let mut patience = Patience::new();
    for cat in all_cats.into_iter() {
        if !cat.needs_download { continue }
        let mut response = match client.get(cat.src_url.clone()).send().await {
            Ok(x) if x.status() == 200 => x,
            Ok(x) => {
                if verbose {
                    gui.borrow_mut().verbose(&format!("failed to download {}", &cat.src_url));
                }
                gui.borrow_mut().do_error("Download failed", &format!("Error \"{}\" while trying to download an updated file.", x.status()));
                return Err(());
            },
            Err(x) => {
                if verbose {
                    gui.borrow_mut().verbose(&format!("failed to download {}", &cat.src_url));
                }
                gui.borrow_mut().do_error("Download failed", &format!("Couldn't download an updated file. The error was:\n{}", x));
                return Err(());
            },
        };
        let _ = std::fs::create_dir_all(cat.dst_path.parent().unwrap());
        let mut f = match File::create(&cat.dst_path) {
            Ok(x) => x,
            Err(x) => {
                gui.borrow_mut().do_error("Update failed", &format!("Couldn't open one of the files we need to update. The path was:\n{:?}\nand the error was:\n{}", cat.dst_path, x));
                return Err(());
            },
        };
        let mut file_recvd_bytes = 0;
        let mut file_hasher = lsx::sha256::BufSha256::new();
        while file_recvd_bytes <= cat.size {
            let now = Instant::now();
            let rate_and_eta = calc_rate_and_eta(start_time, now, total_recvd_bytes, total_cat_bytes);
            if patience.have_been_patient() {
                gui.borrow_mut().set_progress("Downloading updates...", &rate_and_eta, Some(total_recvd_bytes as f32 / total_cat_bytes as f32));
            }
            match response.chunk().await {
                Err(x) => {
                    gui.borrow_mut().do_error("Download failed", &format!("Error while downloading an updated file. The error was:\n{}", x));
                    return Err(());
                },
                Ok(None) => break,
                Ok(Some(x)) => {
                    match f.write_all(&x[..]) {
                        Ok(_) => (),
                        Err(x) => {
                            gui.borrow_mut().do_error("Update failed", &format!("Couldn't write to one of the files we need to update. The path was:\n{:?}\nand the error was:\n{}", cat.dst_path, x));
                            return Err(());
                        },
                    }
                    file_hasher.update(&x[..]);
                    total_recvd_bytes += x.len() as u64;
                    file_recvd_bytes += x.len() as u64;
                },
            }
        }
        let sum = file_hasher.finish(&[]);
        if sum != cat.checksum || file_recvd_bytes != cat.size {
            gui.borrow_mut().do_error("Update failed", &format!("One of the downloads was corrupted. Try running the updater again."));
            return Err(());
        }
    }
    Ok(())
}

fn perform_deletions(gui: &Rc<RefCell<dyn Gui>>, _verbose: bool, all_deletions: Vec<PathBuf>) -> Result<(),()> {
    let num_deletions = all_deletions.len();
    for (n, deletion) in all_deletions.into_iter().enumerate() {
        gui.borrow_mut().set_progress("Deleting obsolete files...", "", Some(n as f32 / num_deletions as f32));
        let is_dir = match std::fs::metadata(&deletion) {
            Ok(x) => x.is_dir(),
            Err(x) if x.kind() == ErrorKind::NotFound => continue,
            Err(x) => {
                gui.borrow_mut().do_error("Error during final deletion", &format!("Unable to get the metadata for {:?}: {}", &deletion, x));
                return Err(())
            }
        };
        let result = if is_dir { std::fs::remove_dir_all(&deletion) } else { std::fs::remove_file(&deletion) };
        if let Err(x) = result {
            gui.borrow_mut().do_error("Error during final deletion", &format!("Unable to delete {:?}: {}", &deletion, x));
            return Err(())
        }
    }
    Ok(())
}

async fn real_main(gui: Rc<RefCell<dyn Gui>>, verbose: bool, target_url: Option<Url>) -> ExitCode {
    let target_url = match find_target_url(&gui, verbose, target_url) {
        Ok(x) => x,
        Err(_) => return ExitCode::FAILURE,
    };
    let mut client = reqwest::Client::builder()
        .user_agent(concat!("TUpdate/", env!("CARGO_PKG_VERSION")))
        //.add_root_certificate(...)
        .build().unwrap();
    let (mut all_cats, mut all_deletions) = match determine_tasks(&gui, verbose, &mut client, &target_url).await {
        Ok(x) => x,
        Err(_) => return ExitCode::FAILURE,
    };
    if find_cat_statuses(&gui, verbose, &mut all_cats).is_err() {
        return ExitCode::FAILURE
    }
    trim_deletions(&gui, verbose, &mut all_cats, &mut all_deletions);
    if perform_downloads(&gui, verbose, &mut client, all_cats).await.is_err() {
        return ExitCode::FAILURE
    }
    if perform_deletions(&gui, verbose, all_deletions).is_err() {
        return ExitCode::FAILURE
    }
    gui.borrow_mut().do_message("Update complete", "All files are now up to date.");
    ExitCode::SUCCESS
}

// hack to prevent Liso from being dropped inside the tokio runtime
fn main() -> ExitCode {
    let Invocation { gui: target_gui, verbose, target_url, pause } = Invocation::parse();
    run_gui(target_gui, pause, move |gui| {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let gui_clone = gui.clone();
        let ret = rt.block_on(async move {
            real_main(gui_clone, verbose, target_url).await
        });
        drop(rt);
        drop(gui);
        ret
    })
}