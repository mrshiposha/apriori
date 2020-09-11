use {
    crate::{
        layer::Layer,
        r#type::{LayerId, LayerName, ObjectName, RawTime, SessionId, SessionInfo, SessionName, TimeFormat},
        storage::{self, StorageManager, StorageTransaction},
        transaction, Result,
    },
    lazy_static::lazy_static,
    log::{error, trace, warn},
    postgres::Transaction,
    ptree::{item::StringItem, TreeBuilder},
};

pub mod context;
pub mod actor;
mod util;

const CONNECTION_STRING: &'static str = "host=localhost user=postgres";
const LOG_TARGET: &'static str = "engine";

lazy_static! {
    static ref ACCESS_UPDATE_TIME: chrono::Duration = chrono::Duration::seconds(30);
    pub static ref SESSION_MAX_HANG_TIME: chrono::Duration =
        chrono::Duration::seconds(ACCESS_UPDATE_TIME.num_seconds() + 10);
}

pub struct Engine {
    storage_mgr: StorageManager,
    session_id: SessionId,
    active_layer_id: LayerId,
    real_time: chrono::Duration,
    last_session_update_time: chrono::Duration,
    virtual_time: chrono::Duration,
    virtual_step: chrono::Duration,
    last_frame_delta: chrono::Duration,
    frames_sum_time_ms: usize,
    frame_count: usize,
}

impl Engine {
    pub fn init() -> Result<Self> {
        let storage_mgr = StorageManager::setup(CONNECTION_STRING, *SESSION_MAX_HANG_TIME)?;

        let session_id = 0;
        let active_layer_id = 0;

        let mut engine = Self {
            storage_mgr,
            session_id,
            active_layer_id,
            real_time: chrono::Duration::zero(),
            last_session_update_time: chrono::Duration::zero(),
            virtual_time: chrono::Duration::zero(),
            virtual_step: chrono::Duration::seconds(1),
            last_frame_delta: chrono::Duration::zero(),
            frames_sum_time_ms: 0,
            frame_count: 0,
        };

        let session_name = None;
        let old_session_id = None;
        engine.new_session_helper(session_name, old_session_id)?;

        Ok(engine)
    }

    pub fn advance_time(&mut self, frame_delta_ns: RawTime, advance_virtual_time: bool) {
        let one_second_ns = chrono::Duration::seconds(1).num_nanoseconds().unwrap();
        let ns_per_ms = 1_000_000;

        let vt_step_ns = self.virtual_step.num_nanoseconds().unwrap();
        let real_step = frame_delta_ns * vt_step_ns / one_second_ns;

        if advance_virtual_time {
            self.virtual_time = self.virtual_time + chrono::Duration::nanoseconds(real_step);
        }

        self.last_frame_delta = chrono::Duration::nanoseconds(frame_delta_ns);
        self.real_time = self.real_time + self.last_frame_delta;

        self.frames_sum_time_ms += (frame_delta_ns / ns_per_ms) as usize;
        self.frame_count += 1;

        self.update_session_access_time().unwrap_or_else(|err| {
            error! {
                target: LOG_TARGET,
                "unable to update the session access time: {}",
                err
            }
        });
    }

    pub fn compute_locations(&mut self) {}

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn virtual_time(&self) -> chrono::Duration {
        self.virtual_time
    }

    pub fn set_virtual_time(&mut self, vtime: chrono::Duration) {
        // TODO load from DB if needed

        self.virtual_time = vtime;
    }

    pub fn virtual_step(&self) -> chrono::Duration {
        self.virtual_step
    }

    pub fn set_virtual_step(&mut self, vstep: chrono::Duration) {
        self.virtual_step = vstep;
    }

    pub fn last_frame_delta(&self) -> chrono::Duration {
        self.last_frame_delta
    }

    pub fn frame(&self) -> usize {
        self.frame_count
    }

    pub fn frame_avg_time_ms(&self) -> f32 {
        self.frames_sum_time_ms as f32 / self.frame_count as f32
    }

    pub fn add_layer(&mut self, layer: Layer) -> Result<()> {
        let session_id = self.session_id;
        let active_layer_id = self.active_layer_id;
        let new_layer_start_time = self.virtual_time;

        let new_layer_id;

        transaction! {
            self.storage_mgr => t(RepeatableRead) {
                new_layer_id = t.layer().add_layer(
                    session_id,
                    active_layer_id,
                    layer.name(),
                    new_layer_start_time
                )?;

                for (object, coord) in layer.take_objects() {
                    let object_id = t.object().add(session_id, new_layer_id, object)?;
                    t.location().add(object_id, new_layer_id, coord)?;
                }
            }
        }

        self.active_layer_id = new_layer_id;

        Ok(())
    }

    pub fn is_object_exists(&mut self, object_name: &ObjectName) -> Result<bool> {
        let result;
        transaction! {
            self.storage_mgr => t {
                result = t.object().is_object_exists(self.session_id, object_name);
            }
        }

        result
    }

    pub fn get_layer_id(&mut self, layer_name: &LayerName) -> Result<LayerId> {
        let result;

        transaction! {
            self.storage_mgr => t {
                result = t.layer().get_layer_id(self.session_id, layer_name);
            }
        }

        result
    }

    pub fn remove_layer(&mut self, layer_name: &LayerName) -> Result<()> {
        transaction! {
            self.storage_mgr => t {
                match self.get_layer_id(layer_name) {
                    Ok(layer_id) => {
                        let mut layer = t.layer();
                        let active_ancestors = layer.layer_ancestors(self.active_layer_id)?;

                        if active_ancestors.contains(&layer_id) {
                            error! {
                                target: LOG_TARGET,
                                "unable to remove active layer or it's ancestors"
                            }
                        } else {
                            layer.remove_layer(layer_id)?;
                        }
                    },
                    Err(err) => warn!("unable to remove a layer: {}", err)
                }
            }
        }

        Ok(())
    }

    pub fn rename_layer(
        &mut self,
        old_layer_name: &LayerName,
        new_layer_name: &LayerName,
    ) -> Result<()> {
        transaction! {
            self.storage_mgr => t {
                let mut layer = t.layer();

                let id = layer.get_layer_id(self.session_id, old_layer_name)?;

                layer.rename_layer(id, new_layer_name)?;
            }
        }

        Ok(())
    }

    pub fn active_layer_name(&mut self) -> Result<LayerName> {
        let id = self.active_layer_id;

        let result;
        transaction! {
            self.storage_mgr => t {
                result = t.layer().get_name(id);
            }
        }

        result
    }

    pub fn current_layer_name(&mut self) -> Result<LayerName> {
        let result;
        transaction! {
            self.storage_mgr => t {
                let mut layer = t.layer();

                let id = self.current_layer_id(&mut layer)?;

                result = layer.get_name(id);
            }
        }

        result
    }

    fn current_layer_id(&mut self, layer_api: &mut storage::Layer) -> Result<LayerId> {
        layer_api.get_current_layer_id(self.active_layer_id, self.virtual_time)
    }

    pub fn get_session_layers(&mut self) -> Result<StringItem> {
        let result;
        transaction! {
            self.storage_mgr => t {
                let session_name = t.session().get_name(self.session_id)?;

                let tree_title = format!("layers of the session \"{}\"", session_name);
                let mut builder = TreeBuilder::new(tree_title);

                let mut layer = t.layer();
                let current_layer_id = self.current_layer_id(&mut layer)?;
                let parent_layer_id = layer.get_main_layer(self.session_id)?;

                self.get_session_layers_helper(
                    &mut layer,
                    &mut builder,
                    current_layer_id,
                    parent_layer_id
                )?;

                result = builder.build();
            }
        }

        Ok(result)
    }

    fn get_session_layers_helper(
        &mut self,
        layer_api: &mut storage::Layer,
        builder: &mut TreeBuilder,
        current_layer_id: LayerId,
        parent_layer_id: LayerId,
    ) -> Result<()> {
        let start_time = layer_api.get_start_time(parent_layer_id)?;

        let layer_name = layer_api.get_name(parent_layer_id)?;
        let layer_status =
            if parent_layer_id == self.active_layer_id && parent_layer_id == current_layer_id {
                "[active/current] "
            } else if parent_layer_id == self.active_layer_id {
                "[active] "
            } else if parent_layer_id == current_layer_id {
                "[current] "
            } else {
                ""
            };

        let layer_info = format!(
            "{}{}: {}",
            layer_status,
            layer_name,
            TimeFormat::VirtualTimeShort(start_time)
        );

        let children = layer_api.get_layer_children(self.session_id, parent_layer_id)?;

        // if children.is_empty() {
        //     builder.add_empty_child(layer_info);
        // } else {
            builder.begin_child(layer_info);

            for &child_id in children.iter() {
                self.get_session_layers_helper(layer_api, builder, current_layer_id, child_id)?;
            }

            builder.end_child();
        // }

        Ok(())
    }

    pub fn select_layer(&mut self, layer_name: &LayerName) -> Result<()> {
        transaction! {
            self.storage_mgr => t {
                let layer_id = t.layer().get_layer_id(self.session_id, layer_name)?;

                if self.active_layer_id != layer_id {
                    self.active_layer_id = layer_id;

                    self.request_simulation_info(&mut t)?;
                }
            }
        }

        Ok(())
    }

    pub fn get_session_name(&mut self) -> Result<SessionName> {
        let result;
        transaction! {
            self.storage_mgr => t {
                result = t.session().get_name(self.session_id);
            }
        }

        result
    }

    pub fn get_sessions_info(&mut self) -> Result<Vec<SessionInfo>> {
        let result;
        transaction! {
            self.storage_mgr => t {
                result = t.session().get_list();
            }
        }

        result
    }

    pub fn new_session(&mut self, session_name: Option<SessionName>) -> Result<()> {
        self.new_session_helper(session_name, Some(self.session_id))
    }

    pub fn save_session(&mut self, session_name: SessionName) -> Result<()> {
        transaction! {
            self.storage_mgr => t {
                t.session().save(self.session_id, &session_name)?;
            }
        }

        Ok(())
    }

    pub fn load_session(&mut self, session_name: SessionName) -> Result<()> {
        transaction! {
            self.storage_mgr => t {
                let mut session = t.session();
                let (new_session_id, new_layer_id) = session.load(&session_name)?;
                self.set_new_session(&mut session, new_session_id, new_layer_id, Some(self.session_id))?;

                self.request_simulation_info(&mut t)?;
            }
        }

        Ok(())
    }

    pub fn rename_session(
        &mut self,
        old_session_name: SessionName,
        new_session_name: SessionName,
    ) -> Result<()> {
        transaction! {
            self.storage_mgr => t {
                t.session().rename(&old_session_name, &new_session_name)?;
            }
        }

        Ok(())
    }

    pub fn delete_session(&mut self, session_name: SessionName) -> Result<()> {
        transaction! {
            self.storage_mgr => t {
                t.session().delete(&session_name)?;
            }
        }

        Ok(())
    }

    fn new_session_helper(
        &mut self,
        session_name: Option<SessionName>,
        old_session_id: Option<SessionId>,
    ) -> Result<()> {
        transaction! {
            self.storage_mgr => t {
                let mut session = t.session();

                let (new_session_id, new_layer_id) = session.new(session_name)?;

                self.set_new_session(&mut session, new_session_id, new_layer_id, old_session_id)?;
            }
        }

        Ok(())
    }

    fn set_new_session(
        &mut self,
        session: &mut storage::Session,
        new_session_id: SessionId,
        new_layer_id: LayerId,
        old_session_id: Option<SessionId>,
    ) -> Result<()> {
        if let Some(old_session_id) = old_session_id {
            session.unlock(old_session_id)?;
        }

        self.session_id = new_session_id;
        self.active_layer_id = new_layer_id;

        Ok(())
    }

    fn update_session_access_time(&mut self) -> Result<()> {
        if self.real_time.num_milliseconds()
            >= (self.last_session_update_time.num_milliseconds()
                + ACCESS_UPDATE_TIME.num_milliseconds())
        {
            trace! {
                target: LOG_TARGET,
                "update session access time"
            };

            transaction! {
                self.storage_mgr => t {
                    t.session().update_access_time(self.session_id)?;
                }
            }

            self.last_session_update_time = self.real_time;
        }

        Ok(())
    }

    fn request_simulation_info(&mut self, _transaction: &mut Transaction) -> Result<()> {
        // TODO load from DB
        Ok(())
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let mut pooled_connection = self
            .storage_mgr
            .pool
            .get()
            .expect("the pooled connection is expected to be established");
        let mut transaction = pooled_connection
            .transaction()
            .expect("the transaction is expected to be started");

        let mut session = storage::Session::new_api(&mut transaction);

        session
            .unlock(self.session_id)
            .expect("the session is expected to be unlocked");

        transaction.commit().unwrap();
    }
}