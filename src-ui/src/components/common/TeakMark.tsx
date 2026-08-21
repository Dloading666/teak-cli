/** Teak CLI brand mark — matches the dock icon: italic T + prompt >_ */
export function TeakMark({
  size = 18,
  className,
}: {
  size?: number;
  className?: string;
}) {
  const height = size;
  const width = Math.round(size * (30 / 24));
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      width={width}
      height={height}
      viewBox="0 0 30 24"
      className={className}
      aria-hidden="true"
    >
      {/* Same silhouette as the app icon: forward-leaning T, wide crossbar. */}
      <g transform="skewX(-13)">
        <path
          fill="currentColor"
          d="M1.4 2.4h15.4v4.5H10.6V21.5H6.4V6.9H1.4z"
        />
      </g>
      <path
        fill="none"
        stroke="currentColor"
        strokeWidth="2.15"
        strokeLinecap="round"
        strokeLinejoin="round"
        d="M18.4 9 22.8 13l-4.4 4"
      />
      <path
        fill="none"
        stroke="currentColor"
        strokeWidth="2.15"
        strokeLinecap="round"
        d="M19.6 19.6h7.2"
      />
    </svg>
  );
}
