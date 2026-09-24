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
