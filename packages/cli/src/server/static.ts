import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { extname, join } from "node:path";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);

function getWebDistPath(): string {
  const webPackagePath = require.resolve("@noctum/web/package.json");
  return join(webPackagePath, "..", "dist");
}

const MIME_TYPES: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "application/javascript",
  ".css": "text/css",
  ".json": "application/json",
  ".png": "image/png",
  ".ico": "image/x-icon",
  ".svg": "image/svg+xml",
};

export interface StaticFileResult {
  content: Buffer;
  contentType: string;
}

export async function serveStaticFile(pathname: string): Promise<StaticFileResult | null> {
  const webDist = getWebDistPath();

  // Default to index.html for root or paths without extension (SPA routing)
  let filePath: string;
  if (pathname === "/" || !extname(pathname)) {
    filePath = join(webDist, "index.html");
  } else {
    filePath = join(webDist, pathname);
  }

  if (!existsSync(filePath)) {
    return null;
  }

  const content = await readFile(filePath);
  const ext = extname(filePath);
  const contentType = MIME_TYPES[ext] || "application/octet-stream";

  return { content, contentType };
}
