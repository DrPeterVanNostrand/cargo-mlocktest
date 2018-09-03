#[cfg(test)]
extern crate memsec;

use std::collections::BTreeMap;
use std::env;
use std::fmt::{self, Display, Formatter};
use std::fs;
use std::iter;
use std::process::{Command, Stdio};
use std::str::{self, FromStr};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::thread::{self, JoinHandle};

// The number of space characters (" ") between table columns.
const COLUMN_BUFFER: usize = 8;

// Ignore child processes with the following names.
const IGNORE_CHILD_PROCS: [&str; 3] = ["rustc", "[rustc]", "rustdoc"];

type Pid = u32;
type Pname = String;

#[derive(Clone, Debug)]
struct Pinfo {
    pname: Pname,
    max_locked: u64,
}

#[derive(Debug)]
struct Database(BTreeMap<Pid, Pinfo>);

impl Database {
    fn new() -> Self {
        Database(BTreeMap::new())
    }

    fn contains(&self, pid: &Pid) -> bool {
        self.0.contains_key(pid)
    }

    fn new_child_process(&mut self, pid: Pid, pname: Pname) {
        self.0.insert(pid, Pinfo { pname, max_locked: 0 });
    }

    fn update(&mut self, pid: Pid, kbs_locked: u64) {
        if let Some(pinfo) = self.0.get_mut(&pid) {
            if kbs_locked > pinfo.max_locked {
                pinfo.max_locked = kbs_locked;
            }
        }
    }

    fn table(&self) -> String {
        let col1_heading = "Process Name";
        let col2_heading = "Max Locked Memory (kb)";
        let col1_heading_len = col1_heading.chars().count();
        let col2_heading_len = col2_heading.chars().count();
        let min_col2_start = col1_heading_len + COLUMN_BUFFER;
        let col2_start = self.0
            .values()
            .fold(min_col2_start, |longest, pinfo| {
                match pinfo.pname.chars().count() + COLUMN_BUFFER {
                    n_chars if n_chars > longest => n_chars,
                    _ => longest,
                }
            });
        let heading_whitespace: String = (0..col2_start - col1_heading_len)
            .map(|_| ' ')
            .collect();
        let heading = format!(
            "{}{}{}",
            col1_heading,
            heading_whitespace,
            col2_heading,
        );
        let top_border = format!(
            "{}{}{}",
            (0..col1_heading_len).map(|_| '=').collect::<String>(),
            heading_whitespace,
            (0..col2_heading_len).map(|_| '=').collect::<String>(),
        );
        let mut stdout = format!("\n{}\n{}\n", heading, top_border);
        for Pinfo { pname, max_locked } in self.0.values() {
            let pname_len = pname.chars().count();
            let whitespace: String = (0..col2_start - pname_len)
                .map(|_| ' ')
                .collect();
            let line = format!("{}{}{}\n", pname, whitespace, max_locked);
            stdout.push_str(&line);
        }
        let table_width = col2_start + col2_heading_len;
        let bottom_border: String = (0..table_width).map(|_| '=').collect();
        stdout.push_str(&bottom_border);
        stdout
    }
}

#[derive(Debug)]
enum Limit {
    Kb(u64),
    Unlimited,
}

impl Display for Limit {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Limit::Kb(kbs) => write!(f, "{}", kbs),
            _ => write!(f, "unlimited"),
        }
    }
}

impl FromStr for Limit {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "unlimited" {
            Ok(Limit::Unlimited)
        } else {
            let n_bytes: u64 = s.parse::<u64>().map_err(|_| ())?;
            Ok(Limit::Kb(n_bytes / 1024))
        }
    }
}

#[derive(Debug)]
struct MlockLimit {
    soft: Limit,
    hard: Limit,
}

fn run_prlimit() -> MlockLimit {
    let output = Command::new("prlimit")
        .args(&["--memlock", "--output=SOFT,HARD", "--noheadings"])
        .output()
        .map(|output| String::from_utf8(output.stdout).unwrap())
        .unwrap_or_else(|e| panic!("Subprocess failed: `ulimit`: {:?}", e));
    let split: Vec<&str> = output.split_whitespace().collect();
    let soft = Limit::from_str(split[0]).unwrap();
    let hard = Limit::from_str(split[1]).unwrap();
    MlockLimit { soft, hard }
}

fn run_ps(cargo_test_pid: Pid) -> Vec<(Pid, Pname)> {
    let mut ps = vec![];
    let ppid = cargo_test_pid.to_string();
    let output = Command::new("ps")
        .args(&["-f", "--ppid", &ppid])
        .output()
        .map(|output| String::from_utf8(output.stdout).unwrap())
        .expect("Subprocess failed: `ps`");
    for line in output.trim().lines().skip(1) {
        let split: Vec<&str> = line.split_whitespace().collect();
        let pid: Pid = split[1].parse().unwrap();
        let pname: Pname = split[7]
            .split_whitespace()
            .nth(0)
            .unwrap()
            .split('/')
            .last()
            .unwrap()
            .to_string();
        if !IGNORE_CHILD_PROCS.contains(&pname.as_ref()) {
            ps.push((pid, pname));
        }
    }
    ps
}

// Launches a thread that continuously calls `ps`, updates the shared
// `child_pids` vector, and inserts the child processes' pids and names
// into the measurements database.
fn launch_ps_thread(
    cargo_test_pid: Arc<Mutex<Option<Pid>>>,
    child_pids: Arc<Mutex<Vec<Pid>>>,
    db: Arc<Mutex<Database>>,
    done: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let cargo_test_pid = loop {
            if let Some(pid) = *cargo_test_pid.lock().unwrap() {
                break pid;
            }
        };
        while !done.load(Ordering::Relaxed) {
            let ps = run_ps(cargo_test_pid);
            *child_pids.lock().unwrap() = ps.iter().map(|(pid, _pname)| *pid).collect();
            let mut db = db.lock().unwrap();
            for (pid, pname) in ps {
                if !db.contains(&pid) {
                    db.new_child_process(pid, pname);
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
    })
}

// Launches a thread that continuously reads each child processes'
// "status" file, parses each file to get the ammount memory locked by that
// child process, then updates the database with the locked memory
// information.
fn launch_measurements_thread(
    cargo_test_pid: Arc<Mutex<Option<Pid>>>,
    child_pids: Arc<Mutex<Vec<Pid>>>,
    db: Arc<Mutex<Database>>,
    done: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        while cargo_test_pid.lock().unwrap().is_none() {
            thread::sleep(Duration::from_millis(1));
        }
        while !done.load(Ordering::Relaxed) {
            for child_pid in child_pids.lock().unwrap().iter() {
                if let Some(kbs_locked) = parse_status_file(*child_pid) {
                    db.lock().unwrap().update(*child_pid, kbs_locked);
                }
            }
            thread::sleep(Duration::from_millis(1));
        }
    })
}

// Reads a processes' "status" file; parsing it for the ammount of memory
// currently locked by the process.
fn parse_status_file(pid: Pid) -> Option<u64> {
    let path = format!("/proc/{}/status", pid);
    let file = fs::read_to_string(path).ok()?;
    for line in file.lines() {
        if line.starts_with("VmLck") {
            match line.trim().split_whitespace().nth(1) {
                Some(s) => return s.parse().ok(),
                _ => return None,
            };
        }
    }
    None
}

fn main() {
    println!("CURRENT CWD => {:?}", env::current_dir());
    println!("CURRENT EXE => {:?}", env::current_exe());

    // Initialize the values that will be shared between threads.
    let cargo_test_pid: Arc<Mutex<Option<Pid>>> = Arc::new(Mutex::new(None));
    let child_pids: Arc<Mutex<Vec<Pid>>> = Arc::new(Mutex::new(vec![]));
    let db = Arc::new(Mutex::new(Database::new()));
    let done = Arc::new(AtomicBool::new(false));

    // Start the worker threads.
    let ps_thread = launch_ps_thread(
        cargo_test_pid.clone(),
        child_pids.clone(),
        db.clone(),
        done.clone()
    );
    let file_reader_thread = launch_measurements_thread(
        cargo_test_pid.clone(),
        child_pids.clone(),
        db.clone(),
        done.clone()
    );

    // Get the system's locked memory limit.
    let mlock_limit = run_prlimit();

    println!("\nMlock Monitor for `cargo test`");
    println!("===============================");
    println!("Locked memory limit (soft, kb): {}", mlock_limit.soft);
    println!("Lock memory limit (hard, kb): {}", mlock_limit.hard);
    print!("\nRunning `cargo test` ... ");

    // Run `cargo test`.
    let cwd = env::current_dir().unwrap();
    let mut cargo_test_args = vec![
        "test".to_string(),
        format!("--manifest-path={}/Cargo.toml", cwd.to_str().unwrap()),
    ];
    cargo_test_args.extend(env::args().skip(1));
    /*
    let mut cargo_test_cmd = Command::new("cargo")
        .args(&cargo_test_args)
        .envs(env::vars())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    println!("Running `cargo test`: {:?}", cargo_test_cmd);
    let cargo_test_output = cargo_test_cmd
        .spawn()
        .and_then(|child| {
            *cargo_test_pid.lock().unwrap() = Some(child.id());
            child.wait_with_output()
        })
        .unwrap();
    */
    let cargo_test_output = Command::new("cargo")
        .args(&cargo_test_args)
        .envs(env::vars())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|child| {
            *cargo_test_pid.lock().unwrap() = Some(child.id());
            child.wait_with_output()
        })  
        .unwrap();

    // Once `cargo test` has finished, stop the worker the threads and
    // print the measurement results.
    println!("done!");
    done.store(true, Ordering::Relaxed);
    let _ = ps_thread.join();
    let _ = file_reader_thread.join();
    println!("{}", db.lock().unwrap().table());

    println!("\nOutput `cargo test`");
    println!("====================");
    println!("{}", String::from_utf8_lossy(&cargo_test_output.stdout));
}

#[cfg(test)]
mod tests {
    use std::mem::size_of_val;
    use std::thread;
    use std::time::Duration;

    use memsec::mlock;

    #[test]
    fn test_mlock() {
        println!("TEST TEST TEST");
        let buf: [u64; 600] = [555; 600];
        let ptr = (&buf).as_ptr() as *mut u8;
        unsafe {
            mlock(ptr, size_of_val(&buf));
        }
        thread::sleep(Duration::from_secs(2));
        assert!(true);
    }
}
