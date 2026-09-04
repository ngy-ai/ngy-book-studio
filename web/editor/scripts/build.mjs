import { build } from "esbuild";
import { buildOptions, outputFile } from "./build-options.mjs";

await build({ ...buildOptions, outfile: outputFile });
