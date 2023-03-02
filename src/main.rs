use std::{
    borrow::Borrow,
    cell::RefCell,
    env::current_exe,
    fs::File,
    io::{Read, BufRead, BufReader, ErrorKind, Write},
    process::ExitCode,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Mutex,
    sync::atomic::{AtomicUsize, Ordering as AtomicOrdering},
};

use clap::Parser;
use rayon::prelude::*;
use url::Url;
use wax::Glob;

mod gui;
use gui::*;

mod update_finder;
use update_finder::find_updates;

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
    #[arg()]
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
    fn try_parse(line: &str, base_url: &Url, base_path: &PathBuf) -> Result<Cat, ()> {
        let split: Vec<&str> = line.split(";").collect();
        if split.len() != 3 { return Err(()) }
        if split[1].len() != 64 { return Err(()) }
        let size = split[2].parse::<u64>().map_err(|_| ())?;
        let file_path = split[0];
        if is_fishy_path(file_path) { return Err(()) }
        let checksum = match hex::decode(split[1]) {
            Ok(x) if x.len() == 32 => { x.try_into().unwrap() },
            _ => return Err(()),
        };
        let src_url = base_url.join(file_path).map_err(|_| ())?;
        Ok(Cat {
            src_url,
            dst_path: base_path.join(file_path),
            checksum,
            size,
            needs_download: false,
        })
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
    for (n, (basedir, caturl)) in installs.iter().enumerate() {
        gui.borrow_mut().set_progress("Downloading update catalogs...", &format!("{}/{} {}", n+1, installs.len(), caturl), Some(n as f32 / installs.len() as f32));
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
        let as_str = match std::str::from_utf8(&body[..]) {
            Ok(x) => x,
            Err(_) => {
                if verbose {
                    gui.borrow_mut().verbose(&format!("bad catalog: {}", caturl));
                }
                gui.borrow_mut().do_error("Invalid catalog", "Found invalid UTF-8 data in an update catalog. Either it was corrupted during the download, or it's corrupted on the far end. Try again in a minute or two.");
                return Err(());
            },
        };
        for line in as_str.lines() {
            if line.is_empty() { continue }
            let cat = match Cat::try_parse(line, caturl, basedir) {
                Ok(x) => x,
                Err(_) => {
                    if verbose {
                        gui.borrow_mut().verbose(&format!("bad catalog: {}", caturl));
                    }
                    gui.borrow_mut().do_error("Invalid catalog", "Found an invalid entry in an update catalog. Either it was corrupted during the download, or it's corrupted on the far end. Try again in a minute or two.");
                    return Err(())
                },
            };
            all_cats.push(cat);
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

async fn perform_downloads(gui: &Rc<RefCell<dyn Gui>>, verbose: bool, client: &mut reqwest::Client, all_cats: Vec<Cat>) -> Result<(),()> {
    let total_cat_bytes = all_cats.iter().fold(0, |a,x| a + if x.needs_download { x.size } else { 0 });
    let mut total_recvd_bytes = 0;
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
        let filename = cat.dst_path.file_name().unwrap().to_string_lossy();
        while file_recvd_bytes <= cat.size {
            gui.borrow_mut().set_progress("Downloading updates...", filename.borrow(), Some(total_recvd_bytes as f32 / total_cat_bytes as f32));
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
    let Invocation { gui: target_gui, verbose, target_url } = Invocation::parse();
    let gui = match create_gui(target_gui) {
        Ok(gui) => gui,
        Err(false) => return ExitCode::SUCCESS,
        Err(true) => return ExitCode::FAILURE,
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let gui_clone = gui.clone();
    let ret = rt.block_on(async move {
        real_main(gui_clone, verbose, target_url).await
    });
    drop(rt);
    drop(gui);
    ret
}