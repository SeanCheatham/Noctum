export interface GitStatusResult {
  isClean: boolean;
  branch: string;
  changedFiles: string[];
}

export interface DirectoryCheckResult {
  exists: boolean;
}

export interface ComposeCheckResult {
  exists: boolean;
  path: string;
}
