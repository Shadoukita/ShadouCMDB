// Lets `node --test` load the app's TypeScript modules as Vite does: relative imports
// without an extension resolve to the `.ts` file. Node strips the types itself.
import { register } from "node:module";

register("./resolve.mjs", import.meta.url);
