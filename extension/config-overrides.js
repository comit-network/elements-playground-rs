const path = require("path");
const WasmPackPlugin = require("@wasm-tool/wasm-pack-plugin");
const webpack = require("webpack");
const fs = require("fs");

module.exports = function override(config, env) {
    config.entry.in_page = path.join(__dirname, "src", "in-page", "index.ts");

    config.resolve.extensions.push(".wasm");

    config.module.rules.forEach(rule => {
        (rule.oneOf || []).forEach(oneOf => {
            if (oneOf.loader && oneOf.loader.indexOf("file-loader") >= 0) {
                // Make file-loader ignore WASM files
                oneOf.exclude.push(/\.wasm$/);
            }
        });
    });

    let outDir = path.resolve(__dirname, "src/background/wallet/generated");

    fs.mkdirSync(outDir, { recursive: true });

    config.plugins = (config.plugins || []).concat([
        new WasmPackPlugin({
            crateDirectory: path.resolve(__dirname, "wallet/"),
            outDir,
        }),
        // delete the warning about "Critical dependency: the request of a dependency is an expression" in the generated binding code
        new webpack.ContextReplacementPlugin(
            /wallet/,
            (data) => {
                delete data.dependencies[0].critical;

                return data;
            },
        ),
    ]);

    return config;
};
