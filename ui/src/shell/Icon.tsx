//! `<Icon name="rec" />` - one of the 56 masks in design/icons.css. Colour is `currentColor`, size
//! is the `--ic` custom property (1em unless a rule or `size` says otherwise).

import type { CSSProperties } from "react";

export function Icon({ name, size, className }: { name: string; size?: number; className?: string }) {
  const style = size ? ({ "--ic": `${size}px` } as CSSProperties) : undefined;
  return <i className={`ic ic-${name}${className ? ` ${className}` : ""}`} style={style} aria-hidden="true" />;
}
