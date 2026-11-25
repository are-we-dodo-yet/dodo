import sheet from "./style.css" with { type: "css" };
import "./side_effect.css" with { type: "css" };

console.log(sheet.constructor.name);
console.log(sheet.cssRules.length, sheet.cssRules[0].cssText);
