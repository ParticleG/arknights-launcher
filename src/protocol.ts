export interface Settings {
  game_dir: string;
  compat_data: string;
  proton_path: string;
}

export interface GameStatus {
  installed: boolean;
  running: boolean;
  verified_version: string | null;
  settings: Settings;
}

export interface ReleaseSummary {
  version: string;
  client_version: string;
  file_count: number;
  compressed_bytes: number;
  installed_bytes: number;
}

export interface LaunchResult {
  already_running: boolean;
}

export interface VerificationSummary {
  version: string;
  files_checked: number;
  missing_files: number;
  changed_files: number;
  verified_bytes: number;
}

export type Operation = "verify" | "install";

export interface OperationProgress {
  operation: Operation;
  phase: string;
  completed_bytes: number;
  total_bytes: number;
  completed_files: number;
  total_files: number;
  current_file: string;
}

export interface OperationFinished {
  operation: Operation;
  cancelled: boolean;
  summary: VerificationSummary | null;
  version: string | null;
  error: string | null;
}

export interface GameExited {
  exit_code: number | null;
}
