import { $ } from "bun";

// 1. Process Tailwind CSS
await $`./node_modules/.bin/tailwindcss -i ./src/index.css -o ./dist/assets/app.css --minify`;

// 2. Bundle with Bun (reads index.html, bundles referenced scripts)
const result = await Bun.build({
  entrypoints: ["./index.html"],
  outdir: "./dist",
  target: "browser",
  minify: true,
  sourcemap: "linked",
});

if (!result.success) {
  console.error("Build failed:");
  for (const msg of result.logs) console.error(msg);
  process.exit(1);
}

console.log("Build succeeded:");
for (const out of result.outputs) {
  console.log(`  ${out.type}: ${out.path} (${(out.size / 1024).toFixed(1)} KB)`);
}
