require("@testing-library/jest-dom");
const { toHaveNoViolations } = require("jest-axe");
const { TextDecoder, TextEncoder } = require("util");

if (typeof global.TextDecoder === "undefined") {
  global.TextDecoder = TextDecoder;
}
if (typeof global.TextEncoder === "undefined") {
  global.TextEncoder = TextEncoder;
}

expect.extend(toHaveNoViolations);

// jsdom's Blob/File does not implement arrayBuffer(); the wasm file loader uses it.
if (typeof Blob !== "undefined" && typeof Blob.prototype.arrayBuffer !== "function") {
  Object.defineProperty(Blob.prototype, "arrayBuffer", {
    configurable: true,
    writable: true,
    value: function arrayBuffer() {
      const blob = this;
      return new Promise((resolve, reject) => {
        const reader = new FileReader();
        reader.addEventListener("load", () => resolve(reader.result));
        reader.addEventListener("error", () => reject(reader.error));
        reader.readAsArrayBuffer(blob);
      });
    },
  });
}
