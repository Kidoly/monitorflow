import styles from "./Logo.module.css";

/**
 * The MonitorFlow logo: a slime in a rounded square. The purple version on
 * a light page, the dark one on a dark page; at 32 px and below, the
 * versions drawn for small sizes (a bigger slime, thicker lines). It is
 * decorative: the name always sits next to it or in the link's label.
 */
export function Logo({ size = 24 }: { size?: number }) {
  const small = size <= 32 ? "-petit" : "";
  return (
    <span className={styles.logo} style={{ width: size, height: size }} aria-hidden="true">
      {/* eslint-disable-next-line @next/next/no-img-element -- static export, no image optimisation */}
      <img className={styles.light} src={`/brand/slime-violette${small}.svg`} alt="" width={size} height={size} />
      {/* eslint-disable-next-line @next/next/no-img-element -- static export, no image optimisation */}
      <img className={styles.dark} src={`/brand/slime-sombre${small}.svg`} alt="" width={size} height={size} />
    </span>
  );
}
