/**
 * Caption designs for the browser preview.
 *
 * The desktop app reads this catalog from the renderer itself so the preview cannot drift from what
 * FFmpeg burns in. Browser preview has no renderer, so it carries a copy generated from that same
 * table (`cargo test -- dump_caption_preset_catalog --ignored --nocapture`).
 */
import type { VideoCaptionPreset } from "../types/video";

export const previewCaptionPresets: VideoCaptionPreset[] = [
  { id: "clean-white", label: "Clean", font_family: "\"Inter Variable\", Inter, system-ui, sans-serif", relative_size: 0.044, text_color: "rgba(255, 255, 255, 1.000)", active_color: "rgba(255, 255, 255, 1.000)", outline_color: "rgba(0, 0, 0, 0.529)", background_color: null, bold: true, letter_spacing_em: 0.0, outline_em: 0.06, casing: "as-is", reveal: "page", max_words_per_page: 8, max_lines: 2 },
  { id: "calm", label: "Calm", font_family: "\"Inter Variable\", Inter, system-ui, sans-serif", relative_size: 0.036, text_color: "rgba(238, 243, 247, 1.000)", active_color: "rgba(238, 243, 247, 1.000)", outline_color: "rgba(17, 17, 17, 0.459)", background_color: null, bold: true, letter_spacing_em: 0.0, outline_em: 0.04, casing: "as-is", reveal: "page", max_words_per_page: 10, max_lines: 2 },
  { id: "kinetic", label: "Kinetic", font_family: "\"Inter Variable\", Inter, system-ui, sans-serif", relative_size: 0.052, text_color: "rgba(255, 255, 255, 1.000)", active_color: "rgba(255, 217, 61, 1.000)", outline_color: "rgba(0, 0, 0, 1.000)", background_color: null, bold: true, letter_spacing_em: 0.02, outline_em: 0.08, casing: "as-is", reveal: "page", max_words_per_page: 4, max_lines: 2 },
  { id: "bold-pop", label: "Bold pop", font_family: "\"Inter Variable\", Inter, system-ui, sans-serif", relative_size: 0.064, text_color: "rgba(255, 255, 255, 1.000)", active_color: "rgba(255, 217, 61, 1.000)", outline_color: "rgba(0, 0, 0, 1.000)", background_color: null, bold: true, letter_spacing_em: 0.02, outline_em: 0.1, casing: "upper", reveal: "active-word", max_words_per_page: 3, max_lines: 2 },
  { id: "highlight", label: "Highlight", font_family: "\"Inter Variable\", Inter, system-ui, sans-serif", relative_size: 0.048, text_color: "rgba(255, 255, 255, 1.000)", active_color: "rgba(79, 118, 242, 1.000)", outline_color: "rgba(0, 0, 0, 0.561)", background_color: null, bold: true, letter_spacing_em: 0.0, outline_em: 0.06, casing: "as-is", reveal: "active-word", max_words_per_page: 5, max_lines: 2 },
  { id: "karaoke", label: "Karaoke", font_family: "\"Inter Variable\", Inter, system-ui, sans-serif", relative_size: 0.047, text_color: "rgba(140, 140, 140, 1.000)", active_color: "rgba(255, 255, 255, 1.000)", outline_color: "rgba(0, 0, 0, 0.529)", background_color: null, bold: true, letter_spacing_em: 0.0, outline_em: 0.06, casing: "as-is", reveal: "karaoke", max_words_per_page: 6, max_lines: 2 },
  { id: "typewriter", label: "Typewriter", font_family: "\"JetBrains Mono Variable\", \"DejaVu Sans Mono\", monospace", relative_size: 0.038, text_color: "rgba(164, 231, 110, 1.000)", active_color: "rgba(164, 231, 110, 1.000)", outline_color: "rgba(0, 0, 0, 0.686)", background_color: "rgba(10, 13, 17, 0.561)", bold: false, letter_spacing_em: 0.02, outline_em: 0.02, casing: "lower", reveal: "typewriter", max_words_per_page: 7, max_lines: 2 },
  { id: "podcast", label: "Podcast", font_family: "\"Inter Variable\", Inter, system-ui, sans-serif", relative_size: 0.039, text_color: "rgba(232, 239, 243, 1.000)", active_color: "rgba(232, 239, 243, 1.000)", outline_color: "rgba(0, 0, 0, 0.686)", background_color: "rgba(16, 21, 26, 0.561)", bold: true, letter_spacing_em: 0.0, outline_em: 0.02, casing: "as-is", reveal: "page", max_words_per_page: 10, max_lines: 2 },
];
