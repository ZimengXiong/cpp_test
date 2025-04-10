use clap::{Arg, Command}; // Removed unused ArgGroup
use notify::{RecommendedWatcher, Watcher, RecursiveMode, Config as NotifyConfig}; // Removed unused EventKind and ModifyKind
use std::process::{Command as ProcessCommand, Stdio};
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::time::{Duration, SystemTime};
use std::fs;
use std::io::{self, Write, Read};
use colored::*;
use chrono;
use std::sync::atomic::{AtomicBool, Ordering}; // For Ctrl+C handling
use std::sync::Arc; // For Ctrl+C handling
use tempfile::{NamedTempFile, TempPath}; // Import TempPath for cleanup

// --- Structs and Enums (TestCase, ParseError) remain the same ---
#[derive(Debug)]
struct TestCase {
    name: String,
    input: String,
    expected_output: String,
}
#[derive(Debug)]
enum ParseError {
    Io(io::Error),
    Format(String),
}
impl From<io::Error> for ParseError {
    fn from(err: io::Error) -> Self {
        ParseError::Io(err)
    }
}
// --- End Structs/Enums ---


// --- Constants for executable names ---
// Keep separate names to avoid clashes between modes
const OUTPUT_WATCH_EXECUTABLE: &str = "./output_watch_run";
const OUTPUT_TEST_EXECUTABLE: &str = "./output_test_watch";
// const OUTPUT_MAIN_STRESS: &str = "./output_main_stress";
// const OUTPUT_GEN_STRESS: &str = "./output_gen_stress";
// const OUTPUT_BRUTE_STRESS: &str = "./output_brute_stress";
// --- End Constants ---

fn main() {
    let matches = Command::new("cpp-watcher")
        .version("1.3") // Incremented version
        .author("Your Name <your.email@example.com>")
        .about("Watches/Tests C++ files. Modes: Watch & Run (-i), Watch & Test (-i -c), Stress Test (-i -g -b).")
        .arg( // Input file (always required)
              Arg::new("input")
                  .short('i')
                  .long("input")
                  .value_name("MAIN_SRC")
                  .help("Sets the main C++ solution file to watch or test")
                  .required(true)
                  .value_parser(clap::value_parser!(String)),
        )
        // --- Test Case File Mode ---
        .arg(
            Arg::new("test-cases")
                .short('c')
                .long("test-cases")
                .value_name("TEST_FILE")
                .help("Continuously runs tests from file, rerunning on changes")
                .required(false)
                .value_parser(clap::value_parser!(String))
                .conflicts_with_all(["generator", "brute"]), // Cannot use with stress test flags
        )
        // --- Stress Test Mode Arguments ---
        .arg(
            Arg::new("generator")
                .short('g')
                .long("generator")
                .value_name("GEN_SRC")
                .help("Generator C++ file for stress testing (requires -b)")
                .required(false)
                .value_parser(clap::value_parser!(String))
                .requires("brute") // If -g is used, -b must also be used
                .conflicts_with("test-cases"), // Cannot use with -c
        )
        .arg(
            Arg::new("brute")
                .short('b')
                .long("brute")
                .value_name("BRUTE_SRC")
                .help("Brute-force/correct C++ solution for stress testing (requires -g)")
                .required(false)
                .value_parser(clap::value_parser!(String))
                .requires("generator") // If -b is used, -g must also be used
                .conflicts_with("test-cases"), // Cannot use with -c
        )
        // --- End Args ---
        .get_matches();

    // --- Get Input File Path (Common to all modes) ---
    let input_file = matches.get_one::<String>("input").unwrap();
    let input_path = Path::new(input_file).to_path_buf();
    validate_cpp_file(&input_path, "Input"); // Use helper for validation

    // --- Mode Selection Logic ---
    if matches.contains_id("generator") { // Stress test mode takes precedence if flags are present
        println!("{}", "Mode: Stress Testing".cyan());
        // We know 'brute' is also present due to 'requires' constraint
        let gen_file = matches.get_one::<String>("generator").unwrap();
        let brute_file = matches.get_one::<String>("brute").unwrap();

        let gen_path = Path::new(gen_file).to_path_buf();
        let brute_path = Path::new(brute_file).to_path_buf();

        validate_cpp_file(&gen_path, "Generator");
        validate_cpp_file(&brute_path, "Brute-force");

        run_stress_test(&input_path, &gen_path, &brute_path);

    } else if matches.contains_id("test-cases") { // Test case file mode
        println!("{}", "Mode: Continuous File Testing".cyan());
        let test_file = matches.get_one::<String>("test-cases").unwrap();
        let test_path = Path::new(test_file).to_path_buf();
        if (!test_path.exists()) {
            eprintln!("{}", format!("Error: Test case file '{}' does not exist.", test_path.display()).red());
            std::process::exit(1);
        }
        println!(
            "{}",
            format!(
                "Continuous test mode: Watching {} and {}",
                input_path.display(),
                test_path.display()
            )
                .dimmed()
        );
        watch_and_test(&input_path, &test_path);

    } else { // Default: Simple watch & run mode
        println!("{}", "Mode: Simple Watch & Run".cyan());
        println!("{}", format!("Watching file: {}", input_path.display()).dimmed());
        watch_and_run(&input_path);
    }
}

// --- Helper to Validate C++ Files ---
fn validate_cpp_file(path: &Path, label: &str) {
    if !path.exists() {
        eprintln!("{}", format!("Error: {} file '{}' does not exist.", label, path.display()).red());
        std::process::exit(1);
    }
    if path.extension().and_then(|ext| ext.to_str()) != Some("cpp") {
        eprintln!("{}", format!("Error: {} file '{}' must be a .cpp file.", label, path.display()).red());
        std::process::exit(1);
    }
}

// --- Helper to Create Temporary Executable Files ---
fn create_temp_executable() -> TempPath {
    NamedTempFile::new()
        .expect("Failed to create temporary file")
        .into_temp_path()
}

// --- Updated Stress Test Function ---
fn run_stress_test(input_path: &Path, gen_path: &Path, brute_path: &Path) {
    let (tx, rx) = channel();
    let mut watcher = match RecommendedWatcher::new(tx, NotifyConfig::default()) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("{} {}", "Failed to create file watcher:".red(), e);
            return;
        }
    };

    if let Err(e) = watcher.watch(input_path, RecursiveMode::NonRecursive) {
        eprintln!("{} {} {}", "Failed to watch file:".red(), input_path.display(), e);
        return;
    }
    if let Err(e) = watcher.watch(gen_path, RecursiveMode::NonRecursive) {
        eprintln!("{} {} {}", "Failed to watch file:".red(), gen_path.display(), e);
        return;
    }
    if let Err(e) = watcher.watch(brute_path, RecursiveMode::NonRecursive) {
        eprintln!("{} {} {}", "Failed to watch file:".red(), brute_path.display(), e);
        return;
    }

    let mut last_input_modified = get_file_modified_time(input_path);
    let mut last_gen_modified = get_file_modified_time(gen_path);
    let mut last_brute_modified = get_file_modified_time(brute_path);

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        if r.load(Ordering::SeqCst) {
            println!("\n{}", "(Ctrl+C detected, stopping stress test...)".yellow());
            r.store(false, Ordering::SeqCst);
        }
    })
    .expect("Error setting Ctrl+C handler");

    println!("{}", "\nStarting stress test loop. Watching for file changes...".green());

    'main_loop: while running.load(Ordering::SeqCst) {
        let main_exec_path = create_temp_executable();
        let gen_exec_path = create_temp_executable();
        let brute_exec_path = create_temp_executable();

        println!("{}", "\nCompiling solutions...".yellow());

        let compiled_main = compile(input_path, &main_exec_path);
        let compiled_gen = compile(gen_path, &gen_exec_path);
        let compiled_brute = compile(brute_path, &brute_exec_path);

        if !compiled_main || !compiled_gen || !compiled_brute {
            eprintln!("{}", "Compilation failed. Cannot start stress test iteration.".red());
        } else {
            println!("{}", "Compilation successful. Starting test iterations...".green());
            let mut seed = 1u64;

            'seed_loop: while running.load(Ordering::SeqCst) {
                print!("{}", format!("\rTesting with seed: {} ", seed).dimmed());
                io::stdout().flush().unwrap_or_default();

                let seed_str = seed.to_string();
                let test_case: String;
                let expected_answer: String;
                let actual_answer: String;

                if !running.load(Ordering::SeqCst) {
                    break 'seed_loop;
                }

                match run_with_input(&gen_exec_path, &seed_str) {
                    Ok(output) => test_case = output,
                    Err(e) => {
                        println!();
                        eprintln!("{}", format!("\nError running generator (seed {}): {}. Skipping seed.", seed, e).red());
                        seed += 1;
                        continue 'seed_loop;
                    }
                }

                if !running.load(Ordering::SeqCst) {
                    break 'seed_loop;
                }

                match run_with_input(&brute_exec_path, &test_case) {
                    Ok(output) => expected_answer = output,
                    Err(e) => {
                        println!();
                        eprintln!("{}", format!("\nError running brute-force (seed {}): {}. Skipping seed.", seed, e).red());
                        seed += 1;
                        continue 'seed_loop;
                    }
                }

                if !running.load(Ordering::SeqCst) {
                    break 'seed_loop;
                }

                match run_with_input(&main_exec_path, &test_case) {
                    Ok(output) => actual_answer = output,
                    Err(e) => {
                        println!();
                        eprintln!("{}", format!("\nError running main solution (seed {}): {}. Skipping seed.", seed, e).red());
                        seed += 1;
                        continue 'seed_loop;
                    }
                }

                if expected_answer.trim() != actual_answer.trim() {
                    println!();
                    println!("{}", format!("\n--- Mismatch Found! (Seed: {}) ---", seed).bright_red().bold());
                    break 'seed_loop;
                }

                seed += 1;
            }
        }

        println!("{}", "\nWaiting for file changes...".dimmed());
        loop {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(event_result) => {
                    if let Ok(event) = event_result {
                        if event.kind.is_modify() || event.kind.is_create() {
                            let current_input_modified = get_file_modified_time(input_path);
                            let current_gen_modified = get_file_modified_time(gen_path);
                            let current_brute_modified = get_file_modified_time(brute_path);

                            if current_input_modified > last_input_modified
                                || current_gen_modified > last_gen_modified
                                || current_brute_modified > last_brute_modified
                            {
                                last_input_modified = current_input_modified;
                                last_gen_modified = current_gen_modified;
                                last_brute_modified = current_brute_modified;
                                break;
                            }
                        }
                    }
                }
                Err(_) => {
                    if !running.load(Ordering::SeqCst) {
                        break 'main_loop;
                    }
                }
            }
        }
    }

    println!("{}", "\nStress test finished.".yellow());
}

// --- Existing Functions (watch_and_run, watch_and_test, compile, run_executable, run_with_input, parse_test_cases, run_tests, get_file_modified_time, timestamp, print_parse_error) ---
// These functions remain the same as in the previous version.
// Make sure `run_tests` still takes `executable_path` and doesn't compile internally.
// (Include the full code for these functions here if needed, or assume they are present from the previous step)
// --- Function for Simple Watch Mode ---
fn watch_and_run(input_path: &Path) {
    let (tx, rx) = channel();
    let mut watcher = match RecommendedWatcher::new(tx, NotifyConfig::default()) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("{} {}", "Failed to create file watcher:".red(), e);
            std::process::exit(1);
        }
    };

    if let Err(e) = watcher.watch(input_path, RecursiveMode::NonRecursive) {
        eprintln!("{} {} {}", "Failed to watch file:".red(), input_path.display(), e);
        std::process::exit(1);
    }

    let mut last_modified = get_file_modified_time(input_path);
    let output_executable = create_temp_executable();

    // --- Handle Ctrl+C for cleanup ---
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        if r.load(Ordering::SeqCst) {
            println!("\n{}", "(Ctrl+C detected, exiting and cleaning up...)".yellow());
            r.store(false, Ordering::SeqCst);
        }
    })
    .expect("Error setting Ctrl+C handler");

    // --- Perform Initial Compile and Run ---
    println!("{}", "\nPerforming initial compile and run...".yellow());
    if compile(input_path, &output_executable) {
        run_executable(&output_executable, None);
    } else {
        println!("{}", "Initial compilation failed.".red());
    }
    println!("{}", "\nWaiting for file changes...".dimmed());
    // --- End Initial Compile and Run ---

    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(event_result) => {
                if let Ok(event) = event_result {
                    if event.kind.is_modify() || event.kind.is_create() {
                        let current_modified = get_file_modified_time(input_path);
                        if current_modified > last_modified {
                            last_modified = current_modified;

                            println!(
                                "{}",
                                format!("\nSource file changed at {}. Recompiling and running...", timestamp())
                                    .yellow()
                            );
                            if compile(input_path, &output_executable) {
                                run_executable(&output_executable, None);
                            } else {
                                println!("{}", "\nCompilation failed.".red());
                            }
                            println!("{}", "\nWaiting for file changes...".dimmed());
                        }
                    }
                } else if let Err(e) = event_result {
                    eprintln!("{}", format!("Watch error: {:?}", e).red());
                }
            }
            Err(_) => {
                if !running.load(Ordering::SeqCst) {
                    break;
                }
                if !input_path.exists() {
                    eprintln!("{}", format!("Error: Watched file '{}' no longer exists. Exiting.", input_path.display()).red());
                    break;
                }
            }
        }
    }

    // Temporary file will be automatically cleaned up when `output_executable` goes out of scope.
}

// --- Function for Continuous Test Mode ---
fn watch_and_test(input_path: &Path, test_path: &Path) {
    let (tx, rx) = channel();
    let mut watcher = match RecommendedWatcher::new(tx, NotifyConfig::default()) {
        Ok(w) => w,
        Err(_e) => {
            eprintln!("{} {}", "Failed to create file watcher:".red(), _e);
            std::process::exit(1);
        }
    };

    if let Err(_e) = watcher.watch(input_path, RecursiveMode::NonRecursive) {
        eprintln!("{} {}", "Failed to watch file:".red(), _e);
        std::process::exit(1);
    }
    if let Err(_e) = watcher.watch(test_path, RecursiveMode::NonRecursive) {
        eprintln!("{} {}", "Failed to watch file:".red(), _e);
        std::process::exit(1);
    }

    let mut last_input_modified = get_file_modified_time(input_path);
    let mut last_test_modified = get_file_modified_time(test_path);
    let output_executable = create_temp_executable();

    // --- Handle Ctrl+C for cleanup ---
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        if r.load(Ordering::SeqCst) {
            println!("\n{}", "(Ctrl+C detected, exiting and cleaning up...)".yellow());
            r.store(false, Ordering::SeqCst);
        }
    })
    .expect("Error setting Ctrl+C handler");

    // --- Perform Initial Compile, Parse, and Test Run ---
    println!("{}", "\nPerforming initial test run...".yellow());
    if compile(input_path, &output_executable) {
        match parse_test_cases(test_path) {
            Ok(test_cases) => {
                if !test_cases.is_empty() {
                    let test_succeeded = run_tests(&output_executable, &test_cases);
                    if test_succeeded {
                        println!("{}", "Initial test run passed.".green());
                    } else {
                        println!("{}", "Initial test run failed.".red());
                    }
                } else {
                    println!("{}", "No test cases found in file.".yellow());
                }
            }
            Err(e) => {
                print_parse_error(&e, test_path);
            }
        }
    } else {
        println!("{}", "Initial compilation failed. Cannot run tests.".red());
    }
    println!("{}", "\nWaiting for file changes...".dimmed());
    // --- End Initial Run ---

    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(event_result) => {
                if let Ok(event) = event_result {
                    if event.kind.is_modify() || event.kind.is_create() {
                        let current_input_modified = get_file_modified_time(input_path);
                        let current_test_modified = get_file_modified_time(test_path);

                        if current_input_modified > last_input_modified
                            || current_test_modified > last_test_modified
                        {
                            last_input_modified = current_input_modified;
                            last_test_modified = current_test_modified;

                            println!(
                                "{}",
                                format!(
                                    "\nChange detected at {}. Recompiling and re-running tests...",
                                    timestamp()
                                )
                                .yellow()
                            );

                            if compile(input_path, &output_executable) {
                                match parse_test_cases(test_path) {
                                    Ok(test_cases) => {
                                        if !test_cases.is_empty() {
                                            run_tests(&output_executable, &test_cases);
                                        } else {
                                            println!("{}", "No test cases found in file.".yellow());
                                        }
                                    }
                                    Err(e) => {
                                        print_parse_error(&e, test_path);
                                    }
                                }
                            } else {
                                println!("{}", "\nCompilation failed. Cannot run tests.".red());
                            }
                            println!("{}", "\nWaiting for file changes...".dimmed());
                        }
                    }
                }
            }
            Err(_) => {
                if !running.load(Ordering::SeqCst) {
                    break;
                }
                if !input_path.exists() || !test_path.exists() {
                    eprintln!("{}", "Error: Watched file no longer exists. Exiting.".red());
                    break;
                }
            }
        }
    }

    // Temporary file will be automatically cleaned up when `output_executable` goes out of scope.
}

// --- Compile Function ---
fn compile(input_path: &Path, output_executable: &Path) -> bool {
    println!("{}", format!("Compiling {} -> {} ...", input_path.display(), output_executable.display()).dimmed());
    let compile_output = ProcessCommand::new("g++")
        .args([
            "-std=c++17", "-Wall", "-Wextra", "-O2", // "-g",
            "-lm", "-o", output_executable.to_str().expect("Output path invalid UTF-8"),
            input_path.to_str().expect("Input path invalid UTF-8"),
        ])
        .output()
        .expect("Failed to execute g++ command");

    if !compile_output.status.success() {
        eprintln!("{}", "-------------------".red());
        eprintln!("{}", "Compilation Failed:".red().bold());
        eprintln!("{}", String::from_utf8_lossy(&compile_output.stderr).trim().red());
        eprintln!("{}", "-------------------".red());
        return false;
    } else if !compile_output.stderr.is_empty() {
        println!("{}", "-------------------".yellow());
        println!("{}", "Compilation Warnings:".yellow().bold());
        println!("{}", String::from_utf8_lossy(&compile_output.stderr).trim().yellow());
        println!("{}", "-------------------".yellow());
    } else { /* Implicit success */ }
    true // Return true only if status is success
}

// --- Function to Run Executable (Simple Watch Mode) ---
fn run_executable(executable_path: &Path, input_data: Option<&str>) -> bool {
    println!("{}", "\nRunning executable...".dimmed());
    let mut command = ProcessCommand::new(executable_path);
    if input_data.is_some() {
        command.stdin(Stdio::piped());
    }
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed spawn {}: {}", executable_path.display(), e);
            return false;
        }
    };

    // --- Input Section ---
    println!("\n{}{}", "Program Input:".bold(), "\n-------------------".dimmed());
    if let Some(input) = input_data {
        println!("{}", input.trim().cyan());
    } else {
        println!("{}", "<No input provided>".dimmed());
    }
    println!("{}", "-------------------".dimmed());

    // --- Write Input to Program ---
    if let Some(input) = input_data {
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(input.as_bytes()) {
                eprintln!("Failed stdin write: {}", e);
            }
            drop(stdin);
        }
    }

    // --- Capture Output ---
    let run_output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Failed wait {}: {}", executable_path.display(), e);
            return false;
        }
    };

    // --- Output Section ---
    println!("\n{}{}", "Program Output:".bold(), "\n-------------------".dimmed());
    let stdout_str = String::from_utf8_lossy(&run_output.stdout);
    if stdout_str.trim().is_empty() {
        println!("{}", "<No standard output>".dimmed());
    } else {
        println!("{}", stdout_str.trim().blue());
    }
    println!("{}", "-------------------\n".dimmed());

    // --- Error Output Section ---
    let stderr_str = String::from_utf8_lossy(&run_output.stderr);
    if !stderr_str.trim().is_empty() {
        eprintln!("{}", "Program Error Output:".yellow().bold());
        eprintln!("{}", "-------------------".yellow());
        eprintln!("{}", stderr_str.trim().yellow());
        eprintln!("{}", "-------------------\n".yellow());
    }

    // --- Check Execution Status ---
    if !run_output.status.success() {
        eprintln!("{}", format!("Execution failed: {}", run_output.status).red());
        return false;
    }
    true
}

// --- Function to Run with Input and Capture Output (Test/Stress Modes) ---
fn run_with_input(executable_path: &Path, input_data: &str) -> Result<String, String> {
    let mut command = ProcessCommand::new(executable_path);
    command.stdin(Stdio::piped()); command.stdout(Stdio::piped()); command.stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|e| format!("Spawn failed '{}': {}", executable_path.display(), e))?;
    let input_data_owned = input_data.to_string();
    let stdin_handle = child.stdin.take().ok_or_else(|| format!("Failed open stdin for {}", executable_path.display()))?;
    let stdin_thread = std::thread::spawn(move || { let mut stdin = stdin_handle; stdin.write_all(input_data_owned.as_bytes()).map_err(|e| format!("Stdin write failed: {}", e)) });

    let mut stdout_output = String::new(); let mut stderr_output = String::new();
    let mut stdout_handle = child.stdout.take().ok_or_else(|| format!("Failed open stdout for {}", executable_path.display()))?;
    let mut stderr_handle = child.stderr.take().ok_or_else(|| format!("Failed open stderr for {}", executable_path.display()))?;

    let stdout_thread = std::thread::spawn(move || { stdout_handle.read_to_string(&mut stdout_output).map_err(|e| format!("Stdout read failed: {}", e))?; Ok::<String, String>(stdout_output) });
    let stderr_thread = std::thread::spawn(move || { stderr_handle.read_to_string(&mut stderr_output).map_err(|e| format!("Stderr read failed: {}", e))?; Ok::<String, String>(stderr_output) });

    let status = child.wait().map_err(|e| format!("Wait failed: {}", e))?;
    match stdin_thread.join() { Ok(Ok(())) => {}, Ok(Err(e)) => return Err(e), Err(_) => return Err("Stdin thread panic".to_string()), }
    let actual_stdout = match stdout_thread.join() { Ok(Ok(out)) => out, Ok(Err(e)) => return Err(e), Err(_) => return Err("Stdout thread panic".to_string()), };
    let actual_stderr = match stderr_thread.join() { Ok(Ok(err)) => err, Ok(Err(e)) => return Err(e), Err(_) => return Err("Stderr thread panic".to_string()), };

    if !status.success() { Err(format!( "Execution failed status: {}. Stderr:\n{}", status, actual_stderr.trim())) }
    else if !actual_stderr.trim().is_empty() { println!("{}", format!("Warning: '{}' produced stderr:\n{}", executable_path.display(), actual_stderr.trim()).yellow()); Ok(actual_stdout) }
    else { Ok(actual_stdout) }
}

// --- Function to Parse Test Cases ---
fn parse_test_cases(test_path: &Path) -> Result<Vec<TestCase>, ParseError> {
    let content = fs::read_to_string(test_path)?;
    let mut test_cases = Vec::new(); let mut lines = content.lines().peekable(); let mut line_number = 0;
    while let Some(line) = lines.next() {
        line_number += 1; let trimmed_line = line.trim();
        if trimmed_line.starts_with("@{") && trimmed_line.ends_with('}') {
            let name = trimmed_line[2..trimmed_line.len() - 1].trim().to_string();
            if name.is_empty() { return Err(ParseError::Format(format!( "Missing test case name line {}", line_number))); }
            let start_line = line_number; let mut input_lines = Vec::new(); let mut expected_output_lines = Vec::new();
            let mut in_input_section = true; let mut found_separator = false;
            while let Some(test_line) = lines.peek() {
                line_number += 1; let trimmed_test_line = test_line.trim();
                if trimmed_test_line == "@" {
                    if !in_input_section { return Err(ParseError::Format(format!( "Unexpected second '@' line {} for test '{}' (started {})", line_number, name, start_line))); }
                    lines.next(); in_input_section = false; found_separator = true;
                } else if trimmed_test_line.starts_with("@{") { break; }
                else { let current_line = lines.next().unwrap(); if in_input_section { input_lines.push(current_line); } else { expected_output_lines.push(current_line); } }
            }
            if !found_separator { return Err(ParseError::Format(format!( "Missing '@' separator for test '{}' (started {})", name, start_line))); }
            test_cases.push(TestCase { name, input: input_lines.join("\n"), expected_output: expected_output_lines.join("\n"), });
        } else if !trimmed_line.is_empty() { return Err(ParseError::Format(format!( "Unexpected content line {}: '{}'", line_number, line))); }
    } Ok(test_cases)
}

// --- Function to Run All Tests (Continuous Test Mode) ---
fn run_tests(executable_path: &Path, test_cases: &[TestCase]) -> bool {
    let mut all_passed = true; let mut passed_count = 0;
    println!("{}", "\n--- Running Tests ---".bold());
    if !executable_path.exists() { eprintln!("Cannot run: Executable '{}' not found.", executable_path.display()); return false; }

    for (index, test_case) in test_cases.iter().enumerate() {
        let status_line = format!( "[{}/{}] Running '{}'... ", index + 1, test_cases.len(), test_case.name );
        print!("{}", status_line.dimmed()); io::stdout().flush().unwrap_or_default();
        match run_with_input(executable_path, &test_case.input) {
            Ok(actual_output_raw) => {
                let actual_output = actual_output_raw.replace("\r\n", "\n").trim().to_string();
                let expected_output = test_case.expected_output.replace("\r\n", "\n").trim().to_string();
                if actual_output == expected_output {
                    println!("{}", "[PASS]".bright_green().bold()); passed_count += 1;
                } else {
                    all_passed = false; println!("{}", "[FAIL]".bright_red().bold());
                    println!("{}:", "Input".cyan()); test_case.input.trim().lines().for_each(|line| println!("  {}", line.cyan()));
                    println!("{}:", "Expected".green()); expected_output.lines().for_each(|line| println!("  {}", line.green()));
                    println!("{}:", "Actual".red()); actual_output.lines().for_each(|line| println!("  {}", line.red()));
                    println!("{}", "--------------------".dimmed());
                }
            }
            Err(err_msg) => {
                all_passed = false; println!("{}", "[ERROR]".bright_red().bold());
                println!("{}:", "Input".cyan()); test_case.input.trim().lines().for_each(|line| println!("  {}", line.cyan()));
                println!("{}:", "Error".red()); err_msg.lines().for_each(|line| println!("  {}", line.red()));
                println!("{}", "--------------------".dimmed());
            }
        }
    }
    println!("{}", "--- Test Summary ---".bold()); println!( "Result: {}/{} tests passed.", passed_count, test_cases.len());
    all_passed
}

// --- Utility Functions ---
fn get_file_modified_time(path: &Path) -> SystemTime { fs::metadata(path).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH) }
fn timestamp() -> String { chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string() }
fn print_parse_error(e: &ParseError, test_path: &Path) {
    eprintln!("{}", "-------------------".red()); eprintln!("{}", "Test File Parsing Failed:".red().bold());
    match e { ParseError::Io(err) => eprintln!("Error reading '{}': {}", test_path.display(), err), ParseError::Format(msg) => eprintln!("Invalid format '{}': {}", test_path.display(), msg), }
    eprintln!("{}", "-------------------".red());
}