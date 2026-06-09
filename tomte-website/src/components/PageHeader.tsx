export function PageHeader({
  kicker,
  title,
  intro,
}: {
  kicker?: string;
  title: string;
  intro: string;
}) {
  return (
    <header className="grain relative overflow-hidden border-b border-line">
      <div className="aurora" aria-hidden="true" />
      <div className="relative mx-auto max-w-[1200px] px-5 pb-14 pt-16 sm:px-8 sm:pb-16 sm:pt-20">
        {kicker ? <span className="mono-label">{kicker}</span> : null}
        <h1 className="mt-4 max-w-4xl text-[2.7rem] leading-[1.0] sm:text-[3.6rem]">
          {title}
        </h1>
        <p className="mt-6 max-w-2xl text-[1.0625rem] leading-relaxed text-ink-2">
          {intro}
        </p>
      </div>
    </header>
  );
}
