#!/usr/bin/env node
import { Command } from "commander";
import { version } from "./version.js";

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

program.parse();
