use std::path::{Path, PathBuf};
use std::collections::HashSet;
use std::io::{self, BufReader, BufRead, ErrorKind};
use std::fs::File;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use tokio::sync:: {Mutex};
use tokio::task::JoinHandle;
use tokio::time::{Duration};
use serde::{Serialize, Deserialize};
use tauri::State;
use futures::future::join_all;

use crate::upload::upload_file;

use crate::media::MediaRecorder;

pub struct RecordingState {
  pub media_process: Option<MediaRecorder>,
  pub upload_handles: Mutex<Vec<JoinHandle<Result<(), String>>>>,
  pub recording_options: Option<RecordingOptions>,
  pub shutdown_flag: Arc<AtomicBool>,
  pub video_uploading_finished: Arc<AtomicBool>,
  pub audio_uploading_finished: Arc<AtomicBool>,
  pub data_dir: Option<PathBuf>
}

unsafe impl Send for RecordingState {}
unsafe impl Sync for RecordingState {}
unsafe impl Send for MediaRecorder {}
unsafe impl Sync for MediaRecorder {}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RecordingOptions {
  pub user_id: String,
  pub video_id: String,
  pub screen_index: String,
  pub video_index: String,
  pub audio_name: String,
  pub aws_region: String,
  pub aws_bucket: String,
}

#[tauri::command]
pub async fn start_dual_recording(
  state: State<'_, Arc<Mutex<RecordingState>>>,
  options: RecordingOptions,
) -> Result<(), String> {
  println!("Starting screen recording...");
  let mut state_guard = state.lock().await;
  
  let shutdown_flag = Arc::new(AtomicBool::new(false));

  let data_dir = state_guard.data_dir.as_ref()
      .ok_or("Data directory is not set in the recording state".to_string())?.clone();

  println!("data_dir: {:?}", data_dir);
  
  let audio_chunks_dir = data_dir.join("chunks/audio");
  let video_chunks_dir = data_dir.join("chunks/video");
  let screenshot_dir = data_dir.join("screenshots");

  clean_and_create_dir(&audio_chunks_dir)?;
  clean_and_create_dir(&video_chunks_dir)?;
  clean_and_create_dir(&screenshot_dir)?;
  
  let audio_name = if options.audio_name.is_empty() {
    None
  } else {
    Some(options.audio_name.clone())
  };
  
  let media_recording_preparation = prepare_media_recording(&options, &audio_chunks_dir, &video_chunks_dir, &screenshot_dir, audio_name);
  let media_recording_result = media_recording_preparation.await.map_err(|e| e.to_string())?;

  state_guard.media_process = Some(media_recording_result);
  state_guard.upload_handles = Mutex::new(vec![]);
  state_guard.recording_options = Some(options.clone());
  state_guard.shutdown_flag = shutdown_flag.clone();
  state_guard.video_uploading_finished = Arc::new(AtomicBool::new(false));
  state_guard.audio_uploading_finished = Arc::new(AtomicBool::new(false));

  let screen_upload = start_upload_loop(video_chunks_dir.clone(), Some(screenshot_dir.clone()), options.clone(), "video".to_string(), shutdown_flag.clone(), state_guard.video_uploading_finished.clone());
  let audio_upload = start_upload_loop(audio_chunks_dir, None, options.clone(), "audio".to_string(), shutdown_flag.clone(), state_guard.audio_uploading_finished.clone());

  drop(state_guard);

  println!("Starting upload loops...");


  match tokio::try_join!(screen_upload, audio_upload) {
      Ok(_) => {
          println!("Both upload loops completed successfully.");
      },
      Err(e) => {
          eprintln!("An error occurred: {}", e);
      },
  }

  Ok(())
}

#[tauri::command]
pub async fn stop_all_recordings(state: State<'_, Arc<Mutex<RecordingState>>>) -> Result<(), String> {
    let mut guard = state.lock().await;
    
    println!("Stopping media recording...");
    
    guard.shutdown_flag.store(true, Ordering::SeqCst);

    if let Some(mut media_process) = guard.media_process.take() {
        println!("Stopping media recording...");
        media_process.stop_media_recording().await.expect("Failed to stop media recording");
    }

    while !guard.video_uploading_finished.load(Ordering::SeqCst) 
        || !guard.audio_uploading_finished.load(Ordering::SeqCst) {
        println!("Waiting for uploads to finish...");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    
    println!("All recordings and uploads stopped.");

    Ok(())
}

fn clean_and_create_dir(dir: &Path) -> Result<(), String> {
    if dir.exists() {
        // Instead of just reading the directory, this will also handle subdirectories.
        std::fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;

    if !dir.to_string_lossy().contains("screenshots") {
      let segment_list_path = dir.join("segment_list.txt");
      match File::open(&segment_list_path) {
          Ok(_) => Ok(()),
          Err(ref e) if e.kind() == ErrorKind::NotFound => {
              File::create(&segment_list_path).map_err(|e| e.to_string())?;
              Ok(())
          },
          Err(e) => Err(e.to_string()), 
      }
    } else {
      Ok(())
    }
}

async fn start_upload_loop(
    chunks_dir: PathBuf,
    screenshot_dir: Option<PathBuf>,
    options: RecordingOptions,
    video_type: String,
    shutdown_flag: Arc<AtomicBool>,
    uploading_finished: Arc<AtomicBool>,
) -> Result<(), String> {
    let mut watched_segments: HashSet<String> = HashSet::new();
    let mut is_final_loop = false;
    let mut screenshot_uploaded = false;

    loop {
        let mut upload_tasks = vec![];
        if shutdown_flag.load(Ordering::SeqCst) {
            if is_final_loop {
                break;
            }
            is_final_loop = true;
        }

        let current_segments = load_segment_list(&chunks_dir.join("segment_list.txt"))
            .map_err(|e| e.to_string())?
            .difference(&watched_segments)
            .cloned()
            .collect::<HashSet<String>>();

        for segment_filename in &current_segments {
            let segment_path = chunks_dir.join(segment_filename);
            if segment_path.is_file() {
                let options_clone = options.clone();
                let video_type_clone = video_type.clone();
                let segment_path_clone = segment_path.clone();
                // Create a task for each file to be uploaded
                upload_tasks.push(tokio::spawn(async move {
                    let filepath_str = segment_path_clone.to_str().unwrap_or_default().to_owned();
                    println!("Uploading video for {}: {}", video_type_clone, filepath_str);
                    upload_file(Some(options_clone), filepath_str, video_type_clone).await.map(|_| ())
                }));
            }
            watched_segments.insert(segment_filename.clone());
        }

        if let Some(screenshot_dir) = &screenshot_dir {
            let screenshot_path = screenshot_dir.join("screen-capture.jpg");
            if !screenshot_uploaded && screenshot_path.is_file() {
                let options_clone = options.clone();
                let video_type_clone = video_type.clone();
                let screenshot_path_clone = screenshot_path.clone();
                upload_tasks.push(tokio::spawn(async move {
                    let filepath_str = screenshot_path_clone.to_str().unwrap_or_default().to_owned();
                    println!("Uploading screenshot for {}: {}", video_type_clone, filepath_str);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    upload_file(Some(options_clone), filepath_str, "screenshot".to_string()).await.map(|_| ())
                }));
                screenshot_uploaded = true;
            }
        }

        // Await all initiated upload tasks in parallel
        if !upload_tasks.is_empty() {
            let _ = join_all(upload_tasks).await;
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    uploading_finished.store(true, Ordering::SeqCst);
    Ok(())
}

fn load_segment_list(segment_list_path: &Path) -> io::Result<HashSet<String>> {
    let file = File::open(segment_list_path)?;
    let reader = BufReader::new(file);

    let mut segments = HashSet::new();
    for line_result in reader.lines() {
        let line = line_result?;
        if !line.is_empty() {
            segments.insert(line);
        }
    }

    Ok(segments)
}

async fn prepare_media_recording(
  options: &RecordingOptions,
  audio_chunks_dir: &Path,
  screenshot_dir: &Path,
  video_chunks_dir: &Path,
  audio_name: Option<String>,
) -> Result<MediaRecorder, String> {
  let mut media_recorder = MediaRecorder::new();
  let audio_file_path = audio_chunks_dir.to_str().unwrap();
  let video_file_path = video_chunks_dir.to_str().unwrap();
  let screenshot_dir_path = screenshot_dir.to_str().unwrap();
  media_recorder.start_media_recording(options.clone(), audio_file_path, screenshot_dir_path, video_file_path, audio_name.as_ref().map(String::as_str)).await?;
  Ok(media_recorder)
}