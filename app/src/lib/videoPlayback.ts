/** Ask the media engine to decode the actual opening frame instead of painting an idle black box. */
export function videoSourceWithFirstFrame(source?: string) {
  if (!source || source.includes("#t=")) return source;
  return `${source}#t=0.001`;
}

export function videoSourceForIdlePoster(source?: string, poster?: string) {
  return poster ? source : videoSourceWithFirstFrame(source);
}
