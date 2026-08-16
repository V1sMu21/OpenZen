/// <reference types="vite/client" />

// Vite `?url` asset imports (pdfjs worker, katex css) carry no types.
declare module "*?url" {
  const url: string;
  export default url;
}
