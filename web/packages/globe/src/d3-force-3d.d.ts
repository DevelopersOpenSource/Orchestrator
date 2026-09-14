// O d3-force-3d não publica tipos; só o que o globo usa.
declare module "d3-force-3d" {
  export interface RadialForce {
    (alpha: number): void;
    strength(value: number): RadialForce;
    radius(value: number): RadialForce;
  }
  export function forceRadial(radius: number, x?: number, y?: number, z?: number): RadialForce;
}
