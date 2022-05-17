extern crate fatfs;
extern crate fscommon;

use xz2::read::XzDecoder;
use git2::Repository;
use std::path::PathBuf;
use std::io::{Write, Read};
use std::fs::File;

use fscommon::BufStream;

fn recursive_copy(host_path: &PathBuf, sd_folder: &mut fatfs::Dir<BufStream<File>>) -> Result<(), std::io::Error> {
    // Iterate over all files in the directory
    for entry in host_path.read_dir()? {
        let entry = entry?;
        let path = entry.path();
        // If the entry is a directory, recurse
        if path.is_dir() {
            let dir_name = path.file_name().unwrap().to_str().unwrap();
            let next_host_path = host_path.join(dir_name);
            let mut next_sd_folder = sd_folder.create_dir(dir_name)?;
            recursive_copy(&next_host_path, &mut next_sd_folder)?;
        } else {
            // Otherwise, copy the file
            let mut file = File::open(path.clone())?;
            let filename = path.file_name().unwrap().to_str().unwrap();
            let mut sd_file = sd_folder.create_file(filename)?;
            let mut buffer = [0; 1024*64];
            loop {
                let bytes_read = file.read(&mut buffer)?;
                if bytes_read == 0 {
                    break;
                }
                sd_file.write_all(&buffer[..bytes_read])?;
            }
            // print
            println!("{}", path.display());
        }
    }
    Ok(())
}

fn pull_repo(dest: &PathBuf) -> Result<(), std::io::Error> {
    let mut repo = Repository::open(dest)?;
    repo.pull(&mut std::io::stdout())?;
    Ok(())
}

fn clone_repo(url: &str, dest: &PathBuf) -> Result<Repository, std::io::Error> {
    let mut repo = Repository::clone(url, dest);
    Ok(repo)
}
    


fn main() -> std::io::Result<()> {

    // Decompress sd.7zip to sd.raw
    let mut sd_raw = File::create("sd.raw")?;
    let mut sd_7zip = XzDecoder::new(File::open("assets/sd.xz")?);
    let mut buffer = [0; 1024*64];
    loop {
        let bytes_read = sd_7zip.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        sd_raw.write_all(&buffer[..bytes_read])?;
    }
    sd_raw.flush()?;

    // check if the /sd_source folder exists
    let sd_source_path = PathBuf::from("sd_source");
    if !sd_source_path.exists() {
        println!("sd_source folder not found");
        std::fs::create_dir(sd_source_path)?;
        return Ok(());
    }
    else {
        println!("sd_source folder found");
    }
    // make /sd_source folder
    std::fs::create_dir("sd_source")?;
    // Clone the Repo to /sd_source folder
    let url = "https://github.com/STulling/BigImageViewer";
    let repo = Repository::clone(url, "sd_source")?;

    // Initialize a filesystem object
    let img_file = std::fs::OpenOptions::new().read(true).write(true).open("sd.raw")?;
    let buf_stream = fscommon::BufStream::new(img_file);
    let fs = fatfs::FileSystem::new(buf_stream, fatfs::FsOptions::new())?;
    let mut root_dir = fs.root_dir();

    // Copy the files
    let cwd = std::env::current_dir()?;
    let host_path = cwd.join("stuff");
    recursive_copy(&host_path, &mut root_dir)?;

    Ok(())
}