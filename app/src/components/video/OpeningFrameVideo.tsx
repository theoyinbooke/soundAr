import { forwardRef, useEffect, useState, type VideoHTMLAttributes } from "react";

export const OpeningFrameVideo = forwardRef<HTMLVideoElement, VideoHTMLAttributes<HTMLVideoElement>>(function OpeningFrameVideo({ poster, src, onPlay, ...props }, ref) {
  const [started, setStarted] = useState(false);
  useEffect(() => setStarted(false), [poster, src]);
  return <>{poster && !started ? <img className="video-opening-poster" src={poster} alt="" aria-hidden="true" /> : null}<video ref={ref} {...props} src={src} poster={poster} onPlay={(event) => { setStarted(true); onPlay?.(event); }} /></>;
});
