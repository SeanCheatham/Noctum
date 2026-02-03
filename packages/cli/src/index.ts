#!/usr/bin/env node
import { Command } from "commander";
import { resolve } from "node:path";
import { version } from "./version.js";
import { startServer } from "./server/index.js";

const program = new Command();

program.name("noctum").description("AI-powered sandboxed simulation testing").version(version);

program
  .command("simulate")
  .description("Run simulation testing in a sandboxed environment")
  .option("-c, --config <path>", "Path to noctum.toml config file", "noctum.toml")
  .option("-t, --timeout <seconds>", "Timeout in seconds", "300")
  .action((options) => {
    console.log("Simulation testing not yet implemented");
    console.log("Options:", options);
  });

program
  .command("serve")
  .description("Start the Noctum web dashboard")
  .option("-p, --port <number>", "Port to listen on (0 for random)", "0")
  .option("--host <address>", "Host to bind to", "127.0.0.1")
  .option("-d, --dir <path>", "Target project directory", process.cwd())
  .action(async (options) => {
    const port = parseInt(options.port, 10);
    const host = options.host;
    // Resolve to absolute path immediately to avoid issues with workspace tools
    const projectDir = resolve(options.dir);

    // Change to the target project directory
    process.chdir(projectDir);

    try {
      const { info, shutdown } = await startServer({ port, host });
      console.log(`Noctum dashboard running at ${info.url}`);
      console.log(`Project directory: ${process.cwd()}`);
      console.log("Press Ctrl+C to stop");

      const handleSignal = async () => {
        console.log("\nShutting down...");
        await shutdown();
        process.exit(0);
      };

      process.on("SIGINT", handleSignal);
      process.on("SIGTERM", handleSignal);
    } catch (err) {
      console.error("Failed to start server:", err);
      process.exit(1);
    }
  });

program.parse();
