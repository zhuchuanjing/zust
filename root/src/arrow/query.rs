use super::hnsw::HNSW;
use super::kv::FjallStore;
use fjall::{KeyspaceCreateOptions, OptimisticTxDatabase, OptimisticTxKeyspace};

pub struct Query {
    pub ids: OptimisticTxKeyspace,          //id -> name
    pub names: OptimisticTxKeyspace,        //name -> id
    pub descriptions: OptimisticTxKeyspace, //name -> description
    pub scales: OptimisticTxKeyspace,       //name -> scale
    pub aabbs: OptimisticTxKeyspace,        //name -> aabb
    pub hnsw: HNSW<FjallStore>,
}

use anyhow::Result;
use indexmap::IndexMap;
use smol_str::SmolStr;

use dynamic::{Dynamic, map};

#[derive(Debug, Clone, PartialEq)]
pub struct SearchDescriptionResult {
    pub name: SmolStr,
    pub description: SmolStr,
    pub scale: SmolStr,
    pub aabb: SmolStr,
    pub distance: f32,
    pub similarity: f32,
}

fn similarity_from_l2_distance(distance: f32) -> f32 {
    1.0 / (1.0 + distance)
}

fn description_results_to_index_map(results: Vec<SearchDescriptionResult>) -> IndexMap<SmolStr, Dynamic> {
    let mut map = IndexMap::with_capacity(results.len());
    for item in results {
        map.insert(
            item.name,
            map!(
                "description" => item.description,
                "scale" => item.scale,
                "aabb" => item.aabb,
                "distance" => item.distance,
                "similarity" => item.similarity
            ),
        );
    }
    map
}

impl Query {
    pub fn new(name: &str) -> Result<Self> {
        Self::new_at(name, name)
    }

    /// Open the database at `path` while preserving a stable logical keyspace name.
    /// This allows a vector database to be moved without creating empty keyspaces.
    pub fn new_at(path: &str, name: &str) -> Result<Self> {
        let db = OptimisticTxDatabase::builder(path).open()?;
        let ids = db.keyspace(&format!("{}_ids", name), KeyspaceCreateOptions::default)?;
        let names = db.keyspace(&format!("{}_names", name), KeyspaceCreateOptions::default)?;
        let descriptions = db.keyspace(&format!("{}_descriptions", name), KeyspaceCreateOptions::default)?;
        let scales = db.keyspace(&format!("{}_scales", name), KeyspaceCreateOptions::default)?;
        let aabbs = db.keyspace(&format!("{}_aabbs", name), KeyspaceCreateOptions::default)?;
        let hnsw = HNSW::<FjallStore>::new(FjallStore::open(&db, &format!("{}_hnsw", name))?, 20, 200, 16, super::Dist::L2);
        Ok(Query { ids, names, descriptions, scales, aabbs, hnsw })
    }

    pub fn rename(&self, name: &str, new_name: &str) -> Result<()> {
        if let Ok(Some(old)) = self.names.get(name) {
            let id = u64::from_le_bytes(old.as_ref().try_into()?);
            self.ids.insert(id.to_le_bytes(), new_name)?;
            self.names.insert(new_name.to_string(), id.to_le_bytes())?;
            if let Some(description) = self.descriptions.get(name)? {
                self.descriptions.insert(new_name.to_string(), description)?;
                self.descriptions.remove(name)?;
            }
            if let Some(scale) = self.scales.get(name)? {
                self.scales.insert(new_name.to_string(), scale)?;
                self.scales.remove(name)?;
            }
            if let Some(aabb) = self.aabbs.get(name)? {
                self.aabbs.insert(new_name.to_string(), aabb)?;
                self.aabbs.remove(name)?;
            }
            self.names.remove(name)?;
        }
        Ok(())
    }

    pub fn add(&self, name: &str, arrow: Vec<f32>) -> Result<u64> {
        self.add_with_description(name, arrow, None)
    }

    pub fn add_with_description(&self, name: &str, arrow: Vec<f32>, description: Option<&str>) -> Result<u64> {
        self.add_with_metadata(name, arrow, description, None, None)
    }

    pub fn add_with_metadata(&self, name: &str, arrow: Vec<f32>, description: Option<&str>, scale: Option<&str>, aabb: Option<&str>) -> Result<u64> {
        if let Ok(Some(old)) = self.names.get(name) {
            let id = u64::from_le_bytes(old.as_ref().try_into()?);
            self.hnsw.set_arrow(id, arrow)?;
            self.update_metadata(name, description, scale, aabb)?;
            Ok(id)
        } else {
            let id = self.hnsw.insert(arrow)?;
            self.ids.insert(id.to_le_bytes(), name)?;
            self.names.insert(name.to_string(), id.to_le_bytes())?;
            self.update_metadata(name, description, scale, aabb)?;
            Ok(id)
        }
    }

    pub fn get_description(&self, name: &str) -> Result<Option<SmolStr>> {
        self.get_text(&self.descriptions, name)
    }

    pub fn get_scale(&self, name: &str) -> Result<Option<SmolStr>> {
        self.get_text(&self.scales, name)
    }

    pub fn get_aabb(&self, name: &str) -> Result<Option<SmolStr>> {
        self.get_text(&self.aabbs, name)
    }

    pub fn search(&self, arrow: Vec<f32>, number: usize) -> Result<Dynamic> {
        let ids = self.hnsw.search(arrow, number)?;
        let mut results = Vec::new();
        for (id, _) in ids {
            let name = self.ids.get(id.to_le_bytes())?.unwrap();
            let s = SmolStr::new(std::str::from_utf8(&name)?);
            results.push(s.into());
        }
        log::info!("hnsw names {:?}", results);
        Ok(Dynamic::list(results))
    }

    pub fn search_description_map(&self, arrow: Vec<f32>, number: usize) -> Result<IndexMap<SmolStr, Dynamic>> {
        let matches = self.search_description_results(arrow, number)?;
        Ok(description_results_to_index_map(matches))
    }

    pub fn search_description_results(&self, arrow: Vec<f32>, number: usize) -> Result<Vec<SearchDescriptionResult>> {
        let ids = self.hnsw.search(arrow, number)?;
        let mut results = Vec::with_capacity(ids.len());
        for (id, distance) in ids {
            let name = self.ids.get(id.to_le_bytes())?.unwrap();
            let name = SmolStr::new(std::str::from_utf8(&name)?);
            let description = self.get_description(name.as_str())?.unwrap_or_default();
            let scale = self.get_scale(name.as_str())?.unwrap_or_default();
            let aabb = self.get_aabb(name.as_str())?.unwrap_or_default();
            results.push(SearchDescriptionResult { name, description, scale, aabb, distance, similarity: similarity_from_l2_distance(distance) });
        }
        Ok(results)
    }

    fn get_text(&self, keyspace: &OptimisticTxKeyspace, name: &str) -> Result<Option<SmolStr>> {
        let value = keyspace.get(name)?;
        value.map(|buf| std::str::from_utf8(&buf).map(SmolStr::new)).transpose().map_err(Into::into)
    }

    fn update_metadata(&self, name: &str, description: Option<&str>, scale: Option<&str>, aabb: Option<&str>) -> Result<()> {
        Self::set_optional_text(&self.descriptions, name, description)?;
        Self::set_optional_text(&self.scales, name, scale)?;
        Self::set_optional_text(&self.aabbs, name, aabb)?;
        Ok(())
    }

    fn set_optional_text(keyspace: &OptimisticTxKeyspace, name: &str, value: Option<&str>) -> Result<()> {
        if let Some(value) = value {
            keyspace.insert(name.to_string(), value)?;
        } else {
            let _ = keyspace.remove(name)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn description_results_to_index_map_preserves_order_and_similarity() {
        let rows = vec![
            SearchDescriptionResult { name: "first".into(), description: "first desc".into(), scale: "".into(), aabb: "".into(), distance: 0.25, similarity: 0.8 },
            SearchDescriptionResult { name: "second".into(), description: "second desc".into(), scale: "".into(), aabb: "".into(), distance: 1.0, similarity: 0.5 },
        ];

        let results = description_results_to_index_map(rows);
        let keys = results.keys().map(|key| key.as_str()).collect::<Vec<_>>();
        let similarity = results.get("first").and_then(|item| item.get_dynamic("similarity")).and_then(|value| value.as_float()).unwrap();

        assert_eq!(keys, vec!["first", "second"]);
        assert!((similarity - 0.8).abs() < 0.0001);
    }
}
