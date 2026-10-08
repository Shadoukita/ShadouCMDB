export async function resolve(specifier, context, next) {
  try {
    return await next(specifier, context);
  } catch (err) {
    if (!/^\.{1,2}\//.test(specifier) || /\.\w+$/.test(specifier)) throw err;
    // A directory ("../i18n") resolves to its index.ts, as Vite resolves it.
    if (err?.code === "ERR_UNSUPPORTED_DIR_IMPORT") return next(`${specifier}/index.ts`, context);
    if (err?.code !== "ERR_MODULE_NOT_FOUND") throw err;
    return next(`${specifier}.ts`, context);
  }
}
