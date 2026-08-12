/** Jod's mascot: a small lion, drawn as line art so it belongs to the HUD.
 *
 *  Line art rather than a filled silhouette for one concrete reason — a filled
 *  face would have to be painted in the panel's background colour to read as a
 *  hole, and the mark sits on three different backgrounds (topbar, palette,
 *  auth gate). An outline has no such dependency: it inherits `currentColor`
 *  and is correct on any of them.
 *
 *  The mane is a ten-scallop ring rather than a spiked star. Both say "lion" at
 *  this size; only one of them is cute.
 */

/** The mane: ten outward arcs around a circle of radius 9, centred on 12,12.
 *  Generated rather than eyeballed — hand-written arc endpoints are how a ring
 *  like this ends up visibly lopsided. */
const MANE =
  "M12 3 A3.45 3.45 0 0 1 17.29 4.72 A3.45 3.45 0 0 1 20.56 9.22 " +
  "A3.45 3.45 0 0 1 20.56 14.78 A3.45 3.45 0 0 1 17.29 19.28 " +
  "A3.45 3.45 0 0 1 12 21 A3.45 3.45 0 0 1 6.71 19.28 " +
  "A3.45 3.45 0 0 1 3.44 14.78 A3.45 3.45 0 0 1 3.44 9.22 " +
  "A3.45 3.45 0 0 1 6.71 4.72 A3.45 3.45 0 0 1 12 3 Z";

interface Props {
  /** Rendered size in pixels. The art is a 24-unit square, so it scales. */
  size?: number;
  /** Extra classes for the call site to position it with. */
  className?: string;
}

export function Mascot({ size = 20, className = "" }: Props) {
  return (
    <svg
      className={`mascot ${className}`.trim()}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeLinecap="round"
      strokeLinejoin="round"
      // A decorative mark beside a wordmark that already says JOD: naming it
      // again would make a screen reader read the brand twice.
      aria-hidden="true"
      focusable="false"
    >
      <path className="mascot-mane" d={MANE} strokeWidth="1.4" />
      <circle className="mascot-face" cx="12" cy="12" r="5.6" strokeWidth="1.1" />
      <circle className="mascot-eye" cx="9.9" cy="11.1" r="1" stroke="none" fill="currentColor" />
      <circle className="mascot-eye" cx="14.1" cy="11.1" r="1" stroke="none" fill="currentColor" />
      <circle className="mascot-nose" cx="12" cy="13.3" r="0.6" stroke="none" fill="currentColor" />
      <path className="mascot-muzzle" d="M10.6 14.4 Q12 15.8 13.4 14.4" strokeWidth="1.1" />
    </svg>
  );
}
