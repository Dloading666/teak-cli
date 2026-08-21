/** Teak CLI brand mark: a T that is also a table (deck + pedestal). */
export function TeakMark({
  size = 18,
  className,
}: {
  size?: number;
  className?: string;
}) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      className={className}
      aria-hidden="true"
    >
      <rect x="2.5" y="5.5" width="19" height="5" rx="1.6" fill="currentColor" />
      <rect x="9.25" y="9.5" width="5.5" height="11" rx="1.6" fill="currentColor" />
    </svg>
  );
}
