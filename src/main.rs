extern crate fatfs;
extern crate fscommon;

use fatfs::{FileSystem, ReadWriteSeek, StdIoWrapper};
use xz2::read::XzDecoder;
use git2::Repository;
use git2::build::{CheckoutBuilder, RepoBuilder};
use git2::{FetchOptions, Progress, RemoteCallbacks};
use std::cell::RefCell;
use std::fmt::Debug;
use std::path::PathBuf;
//use std::io::{Read, BufReader, Write};
use std::fs::File;
use colored::Colorize;

use fscommon::BufStream;

fn debug(msg: &str) {
    let msg = format!("[DEBUG] {}", msg).color(colored::Color::TrueColor { r: 125, g: 125, b: 125 });
    print!("{}", msg);
}

fn error(msg: &str) {
    let msg = format!("[ERROR] {}", msg).color(colored::Color::Red);
    print!("{}", msg);
}

fn warn(msg: &str) {
    let msg = format!("[WARN] {}", msg).color(colored::Color::Yellow);
    print!("{}", msg);
}

fn info(msg: &str) {
    let msg = format!("[INFO] {}", msg).color(colored::Color::White);
    print!("{}", msg);
}

struct State {
    progress: Option<Progress<'static>>,
    total: usize,
    current: usize,
    path: Option<PathBuf>,
    newline: bool,
}

fn print(state: &mut State) {
    let stats = state.progress.as_ref().unwrap();
    let network_pct = (100 * stats.received_objects()) / stats.total_objects();
    let index_pct = (100 * stats.indexed_objects()) / stats.total_objects();
    let co_pct = if state.total > 0 {
        (100 * state.current) / state.total
    } else {
        0
    };
    let kbytes = stats.received_bytes() / 1024;
    if stats.received_objects() == stats.total_objects() {
        if !state.newline {
            println!();
            state.newline = true;
        }
        debug(format!(
            "Resolving deltas {}/{}\r",
            stats.indexed_deltas(),
            stats.total_deltas()
        ).as_str());
    } else {
        debug(format!(
            "downloading {:3}% ({:4} kb, {:5}/{:5})  /  idx {:3}% ({:5}/{:5})  \
             /  chk {:3}% ({:4}/{:4}) {}\r",
            network_pct,
            kbytes,
            stats.received_objects(),
            stats.total_objects(),
            index_pct,
            stats.indexed_objects(),
            stats.total_objects(),
            co_pct,
            state.current,
            state.total,
            state
                .path
                .as_ref()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        ).as_str());
    }
    println!();
}

fn do_fetch<'a>(
    repo: &'a git2::Repository,
    refs: &[&str],
    remote: &'a mut git2::Remote,
) -> Result<git2::AnnotatedCommit<'a>, git2::Error> {
    let mut cb = git2::RemoteCallbacks::new();

    // Print out our transfer progress.
    cb.transfer_progress(|stats| {
        if stats.received_objects() == stats.total_objects() {
            print!(
                "Resolving deltas {}/{}\r",
                stats.indexed_deltas(),
                stats.total_deltas()
            );
        } else if stats.total_objects() > 0 {
            print!(
                "Received {}/{} objects ({}) in {} bytes\r",
                stats.received_objects(),
                stats.total_objects(),
                stats.indexed_objects(),
                stats.received_bytes()
            );
        }
        println!();
        true
    });

    let mut fo = git2::FetchOptions::new();
    fo.remote_callbacks(cb);
    // Always fetch all tags.
    // Perform a download and also update tips
    fo.download_tags(git2::AutotagOption::All);
    println!("Fetching {} for repo", remote.name().unwrap());
    remote.fetch(refs, Some(&mut fo), None)?;

    // If there are local objects (we got a thin pack), then tell the user
    // how many objects we saved from having to cross the network.
    let stats = remote.stats();
    if stats.local_objects() > 0 {
        println!(
            "\rReceived {}/{} objects in {} bytes (used {} local \
             objects)",
            stats.indexed_objects(),
            stats.total_objects(),
            stats.received_bytes(),
            stats.local_objects()
        );
    } else {
        println!(
            "\rReceived {}/{} objects in {} bytes",
            stats.indexed_objects(),
            stats.total_objects(),
            stats.received_bytes()
        );
    }

    let fetch_head = repo.find_reference("FETCH_HEAD")?;
    Ok(repo.reference_to_annotated_commit(&fetch_head)?)
}

fn fast_forward(
    repo: &Repository,
    lb: &mut git2::Reference,
    rc: &git2::AnnotatedCommit,
) -> Result<(), git2::Error> {
    let name = match lb.name() {
        Some(s) => s.to_string(),
        None => String::from_utf8_lossy(lb.name_bytes()).to_string(),
    };
    let msg = format!("Fast-Forward: Setting {} to id: {}", name, rc.id());
    println!("{}", msg);
    lb.set_target(rc.id(), &msg)?;
    repo.set_head(&name)?;
    repo.checkout_head(Some(
        git2::build::CheckoutBuilder::default()
            // For some reason the force is required to make the working directory actually get updated
            // I suspect we should be adding some logic to handle dirty working directory states
            // but this is just an example so maybe not.
            .force(),
    ))?;
    Ok(())
}

fn normal_merge(
    repo: &Repository,
    local: &git2::AnnotatedCommit,
    remote: &git2::AnnotatedCommit,
) -> Result<(), git2::Error> {
    let local_tree = repo.find_commit(local.id())?.tree()?;
    let remote_tree = repo.find_commit(remote.id())?.tree()?;
    let ancestor = repo
        .find_commit(repo.merge_base(local.id(), remote.id())?)?
        .tree()?;
    let mut idx = repo.merge_trees(&ancestor, &local_tree, &remote_tree, None)?;

    if idx.has_conflicts() {
        println!("Merge conficts detected...");
        repo.checkout_index(Some(&mut idx), None)?;
        return Ok(());
    }
    let result_tree = repo.find_tree(idx.write_tree_to(repo)?)?;
    // now create the merge commit
    let msg = format!("Merge: {} into {}", remote.id(), local.id());
    let sig = repo.signature()?;
    let local_commit = repo.find_commit(local.id())?;
    let remote_commit = repo.find_commit(remote.id())?;
    // Do our merge commit and set current branch head to that commit.
    let _merge_commit = repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        &msg,
        &result_tree,
        &[&local_commit, &remote_commit],
    )?;
    // Set working tree to match head.
    repo.checkout_head(None)?;
    Ok(())
}

fn do_merge<'a>(
    repo: &'a Repository,
    remote_branch: &str,
    fetch_commit: git2::AnnotatedCommit<'a>,
) -> Result<bool, git2::Error> {
    // 1. do a merge analysis
    let analysis = repo.merge_analysis(&[&fetch_commit])?;

    // 2. Do the appopriate merge
    if analysis.0.is_fast_forward() {
        println!("Doing a fast forward");
        // do a fast forward
        let refname = format!("refs/heads/{}", remote_branch);
        match repo.find_reference(&refname) {
            Ok(mut r) => {
                fast_forward(repo, &mut r, &fetch_commit)?;
            }
            Err(_) => {
                // The branch doesn't exist so just set the reference to the
                // commit directly. Usually this is because you are pulling
                // into an empty repository.
                repo.reference(
                    &refname,
                    fetch_commit.id(),
                    true,
                    &format!("Setting {} to {}", remote_branch, fetch_commit.id()),
                )?;
                repo.set_head(&refname)?;
                repo.checkout_head(Some(
                    git2::build::CheckoutBuilder::default()
                        .allow_conflicts(true)
                        .conflict_style_merge(true)
                        .force(),
                ))?;
            }
        };
    } else if analysis.0.is_normal() {
        // do a normal merge
        let head_commit = repo.reference_to_annotated_commit(&repo.head()?)?;
        normal_merge(&repo, &head_commit, &fetch_commit)?;
    } else {
        return Ok(false);
    }
    Ok(true)
}

fn pull_repo(repo: &Repository) -> Result<bool, git2::Error> {
    let remote_name = "origin";
    let remote_branch = "main";
    let mut remote = repo.find_remote(remote_name)?;
    let fetch_commit = do_fetch(&repo, &[remote_branch], &mut remote)?;
    do_merge(&repo, &remote_branch, fetch_commit)
}

fn clone_repo(url: &str, path: &PathBuf) -> Result<(), git2::Error> {
    let state = RefCell::new(State {
        progress: None,
        total: 0,
        current: 0,
        path: None,
        newline: false,
    });
    let mut cb = RemoteCallbacks::new();
    cb.transfer_progress(|stats| {
        let mut state = state.borrow_mut();
        state.progress = Some(stats.to_owned());
        print(&mut *state);
        true
    });

    let mut co = CheckoutBuilder::new();
    co.progress(|path, cur, total| {
        let mut state = state.borrow_mut();
        state.path = path.map(|p| p.to_path_buf());
        state.current = cur;
        state.total = total;
        print(&mut *state);
    });

    let mut fo = FetchOptions::new();
    fo.remote_callbacks(cb);
    RepoBuilder::new()
        .fetch_options(fo)
        .with_checkout(co)
        .clone(url, path)?;
    println!();
    Ok(())
}
    
fn err<T, E: Debug>(e: Result<T, E>) -> T {
    match e {
        Ok(t) => t,
        Err(e) => {
            error(format!("{:?}\n", e).as_str());
            std::process::exit(1);
        }
    }
}

fn init_sd() -> Result<(), std::io::Error> {
    // Decompress sd.xz to sd.raw
    info("Decompressing sd.xz to sd.raw\n");
    let mut sd_raw = File::create("sd.raw")?;
    let mut sd_7zip = XzDecoder::new(File::open("assets/sd.xz")?);
    const BUFFERSIZE_MB: usize = 1;
    const BUFFERSIZE: usize = 1024 * 1024 * BUFFERSIZE_MB;
    const SD_SIZE: usize = 1024 * 1024 * 1024 * 2;
    let mut accumulator: usize = 0;
    let mut buffer = vec![0; BUFFERSIZE];
    loop {
        //let bytes_read = sd_7zip.read(&mut buffer)?;
        let bytes_read = std::io::Read::read(&mut sd_7zip, &mut buffer)?;
        accumulator += bytes_read;
        let percentage = (accumulator as f64 / SD_SIZE as f64) * 100.0;
        debug(format!("Progress: {:.1}%\r", percentage).as_str());
        if bytes_read == 0 {
            println!();
            break;
        }
        std::io::Write::write_all(&mut sd_raw, &mut buffer[0..bytes_read])?;
    }
    std::io::Write::flush(&mut sd_raw)?;
    sd_raw.sync_all()?;
    info("Decompressed sd.xz to sd.raw\n");
    Ok(())
}

fn recursive_copy<A: fatfs::TimeProvider, B: fatfs::OemCpConverter, F: ReadWriteSeek>(host_path: &PathBuf, sd_folder: &mut fatfs::Dir<F, A, B>) -> Result<(), std::io::Error> {
    // Iterate over all files in the directory
    for entry in host_path.read_dir()? {
        let entry = entry?;
        let path = entry.path();
        // If the entry starts with a dot, ignore it
        if path.file_name().unwrap().to_str().unwrap().starts_with(".") {
            continue;
        }
        // If the entry is a directory, recurse
        if path.is_dir() {
            let dir_name = path.file_name().unwrap().to_str().unwrap();
            let next_host_path = host_path.join(dir_name);
            let mut next_sd_folder = err(sd_folder.create_dir(dir_name));
            recursive_copy(&next_host_path, &mut next_sd_folder)?;
        } else {
            // Otherwise, copy the file
            let mut file = File::open(path.clone())?;
            let filename = path.file_name().unwrap().to_str().unwrap();
            let mut sd_file = err(sd_folder.create_file(filename));
            // print file creation time
            let mut buffer = vec![0_u8; 1024*1024*8];
            loop {
                let bytes_read = std::io::Read::read(&mut file, &mut buffer)?;
                if bytes_read == 0 {
                    break;
                }
                err(fatfs::Write::write(&mut sd_file, &buffer[..bytes_read]));
            }
            debug(format!("Copying: {}\n", path.display()).as_str());
        }
    }
    Ok(())
}

fn build(sd_source_path: PathBuf) -> Result<(), std::io::Error> {
    // make sd
    info("Building sd.raw\n");
    init_sd()?;
    
    info("Copying the build to sd.raw...\n");
    // Initialize a filesystem object
    let img_file: std::fs::File = std::fs::OpenOptions::new().read(true).write(true).open("sd.raw")?;
    let buf_stream: BufStream<std::fs::File> = fscommon::BufStream::new(img_file);

    let wrapped_buf_stream = StdIoWrapper::from(buf_stream);
    let options = fatfs::FsOptions::new();
    let time_provider = fatfs::NullTimeProvider::new();
    options.time_provider(time_provider);
    let fs: FileSystem<StdIoWrapper<BufStream<File>>, fatfs::NullTimeProvider, fatfs::LossyOemCpConverter> = fatfs::FileSystem::new(wrapped_buf_stream, options)?;
    let mut root_dir = fs.root_dir();

    // Copy the files
    recursive_copy(&sd_source_path, &mut root_dir)?;

    info("Done copying the build to sd.raw\n");
    info("All done!\n");
    Ok(())
}

fn main() -> std::io::Result<()> {

    /* 
    // check if the /sd_source folder exists
    info("Checking if MNN Build already downloaded\n");
    let sd_source_path = PathBuf::from("sd_source");
    let url = "https://github.com/STulling/MNN_Build";
    if !sd_source_path.exists() {
        warn("MNN Build not found\n");
        debug("This is not really a problem, we will now download the build from GitHub\n");
        debug("In future runs, we will update the local MNN Build\n");
        debug("This means that the current download should only ever happen once\n");
        debug("So perhaps sit tight as this may take a while\n");
        info("Downloading MNN Build (can take some time)\n");
        std::fs::create_dir(sd_source_path.clone())?;
        err(clone_repo(url, &sd_source_path.clone()));
        info("Downloaded MNN Build\n");
        build(sd_source_path)?;
    }
    else {
        info("MNN Build found\n");
        info("Checking for updates...\n");
        let repo = err(Repository::open(sd_source_path.clone()));
        let needs_update = err(pull_repo(&repo));
        if needs_update {
            info("MNN Build updated\n");
            build(sd_source_path)?;
        }
        else {
            info("MNN Build is up to date\n");
        }
    }
    info("All done! Launching Dolphin\n");
    */
    let sd_source_path = PathBuf::from("sd_source");
    build(sd_source_path)?;
    Ok(())
}