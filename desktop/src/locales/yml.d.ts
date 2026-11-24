declare module "*.yml" {
  /** Flat object with dotted keys, made by `yamlCatalog` when the page is built. */
  const catalog: Record<string, string | Record<string, string>>;
  export default catalog;
}
