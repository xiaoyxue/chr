//! Inspect a local model without copying it into the repository.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: glb_info <local.glb>")?;
    let asset = chr_renderer::glb::import_glb(&std::fs::read(path)?)?;
    println!(
        "vertices={} triangles={} material_parts={} warnings={}",
        asset.mesh.positions().len(),
        asset.mesh.indices().len() / 3,
        asset.materials.len(),
        asset.warnings.len()
    );
    for warning in asset.warnings {
        eprintln!("{warning}");
    }
    Ok(())
}
