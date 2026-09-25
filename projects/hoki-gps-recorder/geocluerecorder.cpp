// SPDX-License-Identifier: GPL-3.0-only
#include "geocluerecorder.h"
#include <QDBusArgument>
#include <QDBusVariant>
#include <QDBusPendingCallWatcher>
#include <QDBusServiceWatcher>
#include <QDateTime>
#include <QDir>
#include <QJsonArray>
#include <QJsonDocument>
#include <QStandardPaths>
#include <QUuid>
#include <cmath>
#include <ctime>

static const QString service=QStringLiteral("org.freedesktop.Geoclue.Providers.Hybris");
static const QString path=QStringLiteral("/org/freedesktop/Geoclue/Providers/Hybris");
static const QString base=QStringLiteral("org.freedesktop.Geoclue");
static const qint64 limit=128LL*1024*1024;

qint64 GeoClueRecorder::bootMs() {
    timespec t{}; clock_gettime(CLOCK_BOOTTIME, &t);
    return qint64(t.tv_sec)*1000+t.tv_nsec/1000000;
}
int GeoClueRecorder::elapsed() const { return m_startBoot ? int(((m_recording?bootMs():m_endBoot)-m_startBoot)/1000) : 0; }
bool GeoClueRecorder::isFreshTimestamp(qint64 timestamp,qint64 utcNow,qint64 elapsedMs) {
    const qint64 age=utcNow-timestamp;
    // Compare elapsed BOOTTIME, not a fixed wall-clock start that NTP can invalidate.
    return timestamp>0 && age>=-1000 && age<=10000 && age<=elapsedMs+1000;
}
int GeoClueRecorder::fixAge() const { return m_lastFixBoot < 0 ? -1 : int(((m_recording?bootMs():m_endBoot)-m_lastFixBoot)/1000); }

static QJsonValue readArgument(const QDBusArgument &a) {
    QJsonArray values;
    switch(a.currentType()) {
    case QDBusArgument::BasicType: return GeoClueRecorder::encode(a.asVariant());
    case QDBusArgument::VariantType: { QDBusVariant x; a >> x; return GeoClueRecorder::encode(x.variant()); }
    case QDBusArgument::StructureType:
        a.beginStructure(); while(!a.atEnd()) values.append(readArgument(a)); a.endStructure(); return values;
    case QDBusArgument::ArrayType:
        a.beginArray(); while(!a.atEnd()) values.append(readArgument(a)); a.endArray(); return values;
    case QDBusArgument::MapType:
        a.beginMap(); while(!a.atEnd()) { a.beginMapEntry(); QJsonArray pair;
            pair.append(readArgument(a)); pair.append(readArgument(a));
            a.endMapEntry(); values.append(pair); } a.endMap(); return values;
    default: return QJsonObject{{"unsupported_dbus_signature",a.currentSignature()}};
    }
}
QJsonValue GeoClueRecorder::encode(const QVariant &v) {
    if(v.metaType()==QMetaType::fromType<QDBusVariant>()) return encode(qvariant_cast<QDBusVariant>(v).variant());
    if(v.metaType()==QMetaType::fromType<QDBusArgument>()) return readArgument(qvariant_cast<QDBusArgument>(v));
    if(v.metaType().id()==QMetaType::Double) {
        double n=v.toDouble();
        if(std::isnan(n)) return QStringLiteral("NaN");
        if(std::isinf(n)) return n>0?QStringLiteral("+Infinity"):QStringLiteral("-Infinity");
        return n;
    }
    return QJsonValue::fromVariant(v);
}

GeoClueRecorder::GeoClueRecorder(QObject *parent):QObject(parent) {
    m_recovery.setSingleShot(true);
    connect(&m_recovery,&QTimer::timeout,this,&GeoClueRecorder::acquire);
    m_tick.setInterval(1000);
    connect(&m_tick,&QTimer::timeout,this,[this]{
        if(m_bus && !m_bus->isConnected()) { m_error="Session bus disconnected"; finish("bus_disconnected"); return; }
        if(bootMs()>=m_nextHeartbeat) {
            m_nextHeartbeat=bootMs()+10000;
            write({{"event","heartbeat"},{"last_signal_age_ms",m_lastSignalBoot<0?-1:bootMs()-m_lastSignalBoot},
                   {"last_fresh_fix_age_s",fixAge()}});
        }
        emit changed();
    });
}
GeoClueRecorder::~GeoClueRecorder() { finish("app_exit"); }
void GeoClueRecorder::resetFix() { m_lastFixBoot=-1; m_detail="No live fix"; }

void GeoClueRecorder::start() {
    if(m_recording) return;
    m_file.setFileName(QString()); m_startBoot=m_endBoot=0; m_status="Ready";
    m_storageFailed=false; m_signalVersions.clear();
    m_error.clear(); m_recovering=m_acquiring=false; m_recoveryAttempts=0; m_events=0; m_visible=m_used=m_signals=0; resetFix(); m_lastSignalBoot=-1;
    const QString dir=QStandardPaths::writableLocation(QStandardPaths::GenericDataLocation)+"/gps-recordings";
    if(!QDir().mkpath(dir) || !QFile::setPermissions(dir,QFile::ReadOwner|QFile::WriteOwner|QFile::ExeOwner)) {
        m_error="Cannot create private recording directory"; emit changed(); return;
    }
    m_file.setFileName(dir+"/geoclue-"+QDateTime::currentDateTimeUtc().toString("yyyyMMdd'T'HHmmsszzz'Z'")+"-"+
                       QUuid::createUuid().toString(QUuid::Id128)+".jsonl");
    if(!m_file.open(QIODevice::WriteOnly|QIODevice::NewOnly|QIODevice::Unbuffered,QFile::ReadOwner|QFile::WriteOwner)) {
        m_error=m_file.errorString(); emit changed(); return;
    }
    m_startBoot=bootMs(); m_endBoot=0; m_nextHeartbeat=m_startBoot+10000; m_recording=true; ++m_generation;
    m_status="Connecting";
    write({{"event","session_start"},{"schema",1},{"service",service},{"object_path",path},
           {"interval_ms",1000},{"max_bytes",limit},{"clock","CLOCK_BOOTTIME"},
           {"nonfinite_numbers","NaN/+Infinity/-Infinity strings"}});
    if(!m_recording) return;
    m_busName="gps-recorder-"+QUuid::createUuid().toString(QUuid::Id128);
    m_bus=std::make_unique<QDBusConnection>(QDBusConnection::connectToBus(QDBusConnection::SessionBus,m_busName));
    if(!m_bus->isConnected()) { m_error="Session bus unavailable"; finish("bus_error"); return; }
    m_receiver=new RecorderSignalReceiver(this);
    connect(m_receiver,&RecorderSignalReceiver::received,this,&GeoClueRecorder::receive);
    for(const auto &p: QList<QPair<QString,QString>>{{base,"StatusChanged"},{base+".Position","PositionChanged"},
                                                   {base+".Velocity","VelocityChanged"},{base+".Satellite","SatelliteChanged"}}) {
        if(!m_bus->connect(service,path,p.first,p.second,m_receiver,SLOT(receive(QDBusMessage)))) {
            m_error="Cannot subscribe to GeoClue"; finish("subscription_error"); return;
        }
    }
    auto watcher=new QDBusServiceWatcher(service,*m_bus,QDBusServiceWatcher::WatchForOwnerChange,this);
    m_ownerWatcher=watcher;
    connect(watcher,&QDBusServiceWatcher::serviceOwnerChanged,this,&GeoClueRecorder::ownerChanged);
    m_tick.start(); acquire(); emit changed();
}
void GeoClueRecorder::acquire() {
    if(!m_recording) return;
    m_acquiring=true; m_status="Connecting";
    request(base,"AddReference",{},1);
}
void GeoClueRecorder::recover(const QString &reason) {
    if(!m_recording) return;
    m_acquiring=false; m_recovering=true; m_status="Provider offline";
    if(m_recoveryAttempts>=3) {
        m_error="GeoClue unavailable after three recovery attempts"; finish("recovery_exhausted"); return;
    }
    const int delay=2000*(1<<m_recoveryAttempts++);
    write({{"event","provider_recovery"},{"attempt",m_recoveryAttempts},{"delay_ms",delay},{"reason",reason}});
    if(m_recording) m_recovery.start(delay);
    emit changed();
}
void GeoClueRecorder::request(const QString &iface,const QString &method,const QList<QVariant> &args,int continuation) {
    if(!m_recording||!m_bus) return;
    auto message=QDBusMessage::createMethodCall(service,path,iface,method); message.setArguments(args);
    QJsonArray a; for(const auto &v:args) a.append(encode(v));
    write({{"event","method_call"},{"interface",iface},{"member",method},{"arguments",a}});
    if(!m_recording||!m_bus) return;
    const int generation=m_generation;
    const quint64 signalVersion=m_signalVersions.value(iface);
    auto pending=new QDBusPendingCallWatcher(m_bus->asyncCall(message,5000),this);
    connect(pending,&QDBusPendingCallWatcher::finished,this,[this,iface,method,continuation,generation,signalVersion](QDBusPendingCallWatcher *p){
        QDBusMessage reply=p->reply(); p->deleteLater();
        if(!m_recording||generation!=m_generation) return;
        recordMessage(reply,iface,method,"snapshot",signalVersion==m_signalVersions.value(iface));
        if(!m_recording) return;
        if(reply.type()==QDBusMessage::ErrorMessage) {
            m_error=reply.errorName()+": "+reply.errorMessage();
            if(continuation) {
                if(m_recovering) recover(reply.errorName()); else finish("setup_error");
            } else emit changed();
            return;
        }
        if(continuation==1) request(base,"SetOptions",{QVariantMap{{"UpdateInterval",1000}}},2);
        if(continuation==2) {
            m_acquiring=m_recovering=false; m_recoveryAttempts=0; m_recovery.stop(); m_error.clear();
            if(m_status=="Connecting") m_status="Acquiring"; // Preserve an early live StatusChanged.
            request(base,"GetProviderInfo"); request(base,"GetStatus");
            request(base+".Position","GetPosition"); request(base+".Velocity","GetVelocity");
            request(base+".Satellite","GetSatellite"); request(base+".Satellite","GetLastSatellite");
        }
        emit changed();
    });
}
void GeoClueRecorder::ownerChanged(const QString &,const QString &oldOwner,const QString &newOwner) {
    if(!m_recording) return;
    write({{"event","provider_owner_changed"},{"old_owner",oldOwner},{"new_owner",newOwner}});
    if(!m_recording)return; // A storage failure/limit may have stopped us in write().
    // Initial activation already has an AddReference request in flight.
    if(oldOwner.isEmpty()) {
        if(m_recovering && !m_acquiring) { ++m_generation; m_recovery.stop(); acquire(); }
    } else {
        ++m_generation; resetFix(); m_visible=m_used=m_signals=0;
        m_acquiring=false; m_recovering=true; m_recovery.stop();
        if(!newOwner.isEmpty()) acquire(); else recover("provider_owner_lost");
    }
    emit changed();
}
void GeoClueRecorder::receive(const QDBusMessage &message) {
    if(!m_recording) return;
    m_lastSignalBoot=bootMs(); ++m_signalVersions[message.interface()];
    recordMessage(message,message.interface(),message.member(),"signal");
}
void GeoClueRecorder::recordMessage(const QDBusMessage &message,const QString &iface,const QString &member,const QString &source,bool applyToUi) {
    QJsonArray args; for(const auto &v:message.arguments()) args.append(encode(v));
    QJsonObject event{{"event",message.type()==QDBusMessage::ErrorMessage?"dbus_error":"dbus"},
                      {"source",source},{"sender",message.service()},{"interface",iface},{"member",member},
                      {"signature",message.signature()},{"arguments",args},{"applied_to_ui",applyToUi && member!="GetLastSatellite"}};
    if(message.type()==QDBusMessage::ErrorMessage) {
        event["error_name"]=message.errorName(); event["error_message"]=message.errorMessage();
    } else if(applyToUi && iface==base && (member=="GetStatus"||member=="StatusChanged") && args.size()==1) {
        const int status=args[0].toInt(-1);
        m_status=QStringList{"Error","Unavailable","Acquiring","Available"}.value(status,"Unknown status");
        if(status!=3) resetFix();
    } else if(applyToUi && iface==base+".Satellite" && member!="GetLastSatellite" && args.size()==5) {
        m_used=args[1].toInt(); m_visible=args[2].toInt(); m_signals=0;
        for(const auto &s:args[4].toArray()) { auto sat=s.toArray(); if(sat.size()==4 && sat[3].toInt()>0) ++m_signals; }
    } else if(iface==base+".Position" && args.size()==6) {
        const int fields=args[0].toInt();
        const qint64 timestamp=qint64(args[1].toDouble())*1000;
        const qint64 age=QDateTime::currentMSecsSinceEpoch()-timestamp;
        const bool valid=(fields&3)==3 && args[2].isDouble() && args[3].isDouble()
            && std::abs(args[2].toDouble())<=90 && std::abs(args[3].toDouble())<=180;
        const bool fresh=source=="signal" && valid && isFreshTimestamp(timestamp,QDateTime::currentMSecsSinceEpoch(),bootMs()-m_startBoot);
        event["fresh_for_session"]=fresh;
        if(applyToUi && fresh) {
            m_lastFixBoot=bootMs()-qMax<qint64>(0,age);
            const auto accuracy=args[5].toArray();
            m_detail=accuracy.size()==3 && accuracy[1].isDouble()
                ? QString("Accuracy %1 m").arg(accuracy[1].toDouble(),0,'f',1) : "Live fix";
        } else if(applyToUi && !valid) resetFix();
    }
    write(event); emit changed();
}
void GeoClueRecorder::write(QJsonObject event) {
    if(!m_recording||!m_file.isOpen()||m_storageFailed) return;
    if(!m_finishing && m_file.size()>limit-65536) { m_error="Recording reached 128 MiB limit"; finish("size_limit"); return; }
    event["sequence"]=m_events+1;
    event["received_utc"]=QDateTime::currentDateTimeUtc().toString(Qt::ISODateWithMs);
    event["boottime_ms"]=bootMs(); event["elapsed_ms"]=bootMs()-m_startBoot;
    auto line=QJsonDocument(event).toJson(QJsonDocument::Compact)+'\n';
    if(m_file.write(line)!=line.size() || !m_file.flush()) {
        m_storageFailed=true; // Never append an end marker after a partial JSON line.
        m_error="Recording write failed: "+m_file.errorString();
        if(!m_finishing) finish("write_error");
    } else ++m_events;
}
void GeoClueRecorder::stop() { finish("user_stop"); }
void GeoClueRecorder::finish(const QString &reason) {
    if(!m_recording||m_finishing) return;
    m_finishing=true;
    write({{"event","session_end"},{"reason",reason},{"error",m_error},
           {"reference_release","disconnect dedicated D-Bus client"}});
    m_endBoot=bootMs(); m_recording=false; ++m_generation; m_tick.stop(); m_recovery.stop();
    if(m_ownerWatcher) { m_ownerWatcher->disconnect(this); m_ownerWatcher->deleteLater(); m_ownerWatcher=nullptr; }
    if(m_receiver) { m_receiver->disconnect(this); m_receiver->deleteLater(); m_receiver=nullptr; }
    if(m_bus) { QDBusConnection::disconnectFromBus(m_busName); m_bus.reset(); }
    m_file.close(); m_status=m_error.isEmpty()?"Saved":"Recording stopped";
    m_finishing=false; emit changed();
}
