// SPDX-License-Identifier: GPL-3.0-only
#include "geocluerecorder.h"
#include <QCoreApplication>
#include <QDBusVirtualObject>
#include <QDBusArgument>
#include <QDBusMetaType>
#include <QDBusConnectionInterface>
#include <QDateTime>
#include <QDir>
#include <QJsonArray>
#include <QJsonDocument>
#include <QTemporaryDir>
#include <QDebug>
#include <limits>
#include <sys/resource.h>
#include <signal.h>

const QString service="org.freedesktop.Geoclue.Providers.Hybris";
const QString path="/org/freedesktop/Geoclue/Providers/Hybris";
const QString base="org.freedesktop.Geoclue";
struct Sat { int prn, elevation, azimuth, snr; };
Q_DECLARE_METATYPE(Sat)
Q_DECLARE_METATYPE(QList<Sat>)
QDBusArgument &operator<<(QDBusArgument &a,const Sat &s) { a.beginStructure(); a<<s.prn<<s.elevation<<s.azimuth<<s.snr; a.endStructure(); return a; }
const QDBusArgument &operator>>(const QDBusArgument &a,Sat &s) { a.beginStructure(); a>>s.prn>>s.elevation>>s.azimuth>>s.snr; a.endStructure(); return a; }
static void check(bool ok,const char *what) { if(!ok) qFatal("FAIL: %s",what); }
static double nan() { return std::numeric_limits<double>::quiet_NaN(); }
QList<QVariant> position(int fields,int timestamp) {
    QDBusArgument accuracy; accuracy.beginStructure(); accuracy<<6<<4.5<<nan(); accuracy.endStructure();
    return {fields,timestamp,fields?12.0:nan(),fields?34.0:nan(),nan(),QVariant::fromValue(accuracy)};
}
QList<QVariant> satellites(int timestamp) {
    return {timestamp,1,2,QVariant::fromValue(QList<int>{64}),QVariant::fromValue(QList<Sat>{{1,10,20,0},{64,40,50,22}})};
}
class Provider:public QDBusVirtualObject {
public:
    bool delaySnapshots=false, delayOldSession=false, earlyStatus=false;
    int refs=0;
    QString lastClient;
    QString introspect(const QString &) const override { return {}; }
    bool handleMessage(const QDBusMessage &m,const QDBusConnection &bus) override {
        QList<QVariant> args; int now=QDateTime::currentSecsSinceEpoch();
        if(m.member()=="AddReference") { ++refs; lastClient=m.service(); }
        else if(m.member()=="GetProviderInfo") args={"synthetic-test", "isolated recorder test"};
        else if(m.member()=="GetStatus") {
            if(earlyStatus) {bus.send(m.createErrorReply("org.freedesktop.Geoclue.Error.NotAvailable","synthetic status error"));return true;}
            args={2};
        }
        else if(m.member()=="GetPosition") args=position(3,now-60);
        else if(m.member()=="GetVelocity") {
            if(refs==2) { bus.send(m.createErrorReply("org.freedesktop.Geoclue.Error.NotAvailable","synthetic velocity error")); return true; }
            args={0,now,nan(),nan(),nan()};
        }
        else if(m.member()=="GetSatellite") args=satellites(now);
        else if(m.member()=="GetLastSatellite") args=satellites(now-20);
        if(earlyStatus && m.member()=="SetOptions") {
            send(base,"StatusChanged",{3});
            const auto reply=m.createReply(args); QTimer::singleShot(150,this,[bus,reply]{bus.send(reply);});return true;
        }
        if((delaySnapshots || (delayOldSession && refs==1)) && m.member().startsWith("Get")) {
            const auto reply=m.createReply(args);
            QTimer::singleShot(500,this,[bus,reply]{bus.send(reply);});
        } else bus.send(m.createReply(args));
        return true;
    }
    void send(const QString &iface,const QString &name,const QList<QVariant> &args) {
        auto m=QDBusMessage::createSignal(path,iface,name); m.setArguments(args); QDBusConnection::sessionBus().send(m);
    }
};
int main(int argc,char **argv) {
    QCoreApplication app(argc,argv);
    if(argc>1 && QString::fromLocal8Bit(argv[1])=="--live") {
        GeoClueRecorder recorder;
        recorder.start();
        const int seconds=argc>2?QString::fromLocal8Bit(argv[2]).toInt():20;
        check(seconds>=1 && seconds<=120,"bounded live duration");
        QTimer::singleShot(seconds*1000,&app,[&]{ recorder.stop(); check(recorder.error().isEmpty(),"live recorder error");
            qInfo()<<"LIVE_PASS"<<recorder.events()<<"events"<<recorder.fileName(); app.quit(); });
        return app.exec();
    }
    QTemporaryDir temporary;
    check(temporary.isValid(),"temporary data directory"); qputenv("XDG_DATA_HOME",temporary.path().toUtf8());
    qDBusRegisterMetaType<Sat>(); qDBusRegisterMetaType<QList<Sat>>(); qDBusRegisterMetaType<QList<int>>();
    check(GeoClueRecorder::isFreshTimestamp(100000,100500,20000),"fresh timestamp after backward clock correction");
    check(!GeoClueRecorder::isFreshTimestamp(88000,100500,20000),"old fix rejected");
    check(!GeoClueRecorder::isFreshTimestamp(97000,100500,100),"pre-session fix rejected");
    Provider provider;
    const QString mode=argc>1?QString::fromLocal8Bit(argv[1]):QString();
    provider.delaySnapshots=mode=="--stale-snapshot";
    provider.delayOldSession=mode=="--queued-session";
    provider.earlyStatus=mode=="--early-status";
    auto bus=QDBusConnection::sessionBus();
    check(bus.registerVirtualObject(path,&provider),"register test object");
    check(bus.registerService(service),"register isolated test service");
    GeoClueRecorder recorder; recorder.start();
    if(mode=="--freeze-age") {
        int finalAge=-1, finalElapsed=-1;
        QTimer::singleShot(200,&app,[&]{provider.send(base+".Position","PositionChanged",position(3,QDateTime::currentSecsSinceEpoch()));});
        QTimer::singleShot(300,&app,[&]{
            check(recorder.fixAge()>=0,"fresh position before stop");recorder.stop();finalAge=recorder.fixAge();finalElapsed=recorder.elapsed();
        });
        QTimer::singleShot(1800,&app,[&]{
            check(recorder.fixAge()==finalAge&&recorder.elapsed()==finalElapsed,"stopped session ages stay frozen");
            qInfo()<<"PASS: final fix age and elapsed time freeze on stop";app.quit();
        });
        return app.exec();
    }
    if(mode=="--early-status") {
        QTimer::singleShot(500,&app,[&]{
            check(recorder.status()=="Available","setup acknowledgement preserves early live status");
            check(recorder.error().contains("synthetic status error"),"GetStatus failure exercised");
            recorder.stop();qInfo()<<"PASS: setup acknowledgement cannot overwrite early live provider status";app.quit();
        });
        return app.exec();
    }
    if(mode=="--queued-session") {
        QString oldClient;
        QTimer::singleShot(200,&app,[&]{
            oldClient=provider.lastClient;
            auto *receiver=recorder.findChild<RecorderSignalReceiver*>(); check(receiver,"per-session receiver");
            auto m=QDBusMessage::createSignal(path,base+".Satellite","SatelliteChanged");
            auto args=satellites(QDateTime::currentSecsSinceEpoch());args[2]=99;m.setArguments(args);
            QMetaObject::invokeMethod(receiver,"receive",Qt::QueuedConnection,Q_ARG(QDBusMessage,m));
            recorder.stop(); recorder.start();
        });
        QTimer::singleShot(350,&app,[&]{
            check(!bus.interface()->isServiceRegistered(oldClient).value(),"stopped client disconnects despite pending snapshots");
        });
        QTimer::singleShot(850,&app,[&]{
            check(recorder.visible()!=99,"queued prior-session signal ignored");
            recorder.stop();
            QFile file(temporary.path()+"/gps-recordings/"+recorder.fileName());check(file.open(QIODevice::ReadOnly),"new session log");
            while(!file.atEnd()) { auto row=QJsonDocument::fromJson(file.readLine()).object();
                if(row["event"]=="dbus" && row["member"]=="SatelliteChanged")
                    check(row["arguments"].toArray()[2].toInt()!=99,"old signal not logged in new session");
            }
            qInfo()<<"PASS: rapid stop/start isolates queued signals and releases pending-call client";app.quit();
        });
        return app.exec();
    }
    if(mode=="--stale-snapshot") {
        QTimer::singleShot(200,&app,[&]{
            provider.send(base+".Position","PositionChanged",position(3,QDateTime::currentSecsSinceEpoch()));
            provider.send(base,"StatusChanged",{3});
            auto args=satellites(QDateTime::currentSecsSinceEpoch()); args[2]=7;
            provider.send(base+".Satellite","SatelliteChanged",args);
        });
        QTimer::singleShot(850,&app,[&]{
            check(recorder.fixAge()>=0 && recorder.status()=="Available" && recorder.visible()==7,"late snapshots cannot replace live state");
            recorder.stop();
            QFile f(temporary.path()+"/gps-recordings/"+recorder.fileName()); check(f.open(QIODevice::ReadOnly),"read delayed snapshots");
            int retained=0;
            while(!f.atEnd()) { auto row=QJsonDocument::fromJson(f.readLine()).object();
                if(row["event"]=="dbus" && (row["member"]=="GetStatus"||row["member"]=="GetSatellite"||row["member"]=="GetPosition")) {
                    check(!row["applied_to_ui"].toBool(),"obsolete snapshot labelled"); ++retained;
                }
            }
            check(retained==3,"all late snapshots retained in log");
            qInfo()<<"PASS: delayed snapshots retained without regressing live UI"; app.quit();
        });
        return app.exec();
    }
    if(mode=="--size-limit") {
        QTimer::singleShot(200,&app,[&]{
            QFile file(temporary.path()+"/gps-recordings/"+recorder.fileName());
            check(file.open(QIODevice::ReadWrite)&&file.resize(128LL*1024*1024),"sparse file limit fixture"); file.close();
            bus.unregisterService(service);
        });
        QTimer::singleShot(500,&app,[&]{
            check(!recorder.recording()&&recorder.error().contains("128 MiB"),"size limit stops recording");
            check(recorder.status()=="Recording stopped","owner change must not override storage stop state");
            qInfo()<<"PASS: file limit during owner loss keeps final stop state"; app.quit();
        });
        return app.exec();
    }
    if(mode=="--partial-write") {
        rlimit saved{}; check(getrlimit(RLIMIT_FSIZE,&saved)==0,"read file limit");
        QTimer::singleShot(200,&app,[&]{
            QFile f(temporary.path()+"/gps-recordings/"+recorder.fileName());
            rlimit small=saved; small.rlim_cur=f.size()+15; signal(SIGXFSZ,SIG_IGN);
            check(setrlimit(RLIMIT_FSIZE,&small)==0,"set partial write limit");
            provider.send(base+".Satellite","SatelliteChanged",satellites(QDateTime::currentSecsSinceEpoch()));
        });
        QTimer::singleShot(500,&app,[&]{
            check(setrlimit(RLIMIT_FSIZE,&saved)==0,"restore file limit");
            check(!recorder.recording()&&recorder.error().contains("write failed"),"storage failure stops recording");
            QFile f(temporary.path()+"/gps-recordings/"+recorder.fileName()); check(f.open(QIODevice::ReadOnly),"read partial write");
            int partial=0;
            while(!f.atEnd()) { auto line=f.readLine(); QJsonParseError error; QJsonDocument::fromJson(line,&error);
                if(error.error!=QJsonParseError::NoError) {++partial;check(f.atEnd()&&!line.endsWith('\n'),"only final line torn");}
                check(!line.contains("session_end"),"no marker appended to torn row");
            }
            check(partial<=1,"recoverable JSON prefix");
            qInfo()<<"PASS: real partial write stops acquisition and preserves parseable prefix"; app.quit();
        });
        return app.exec();
    }
    if(argc>1 && QString::fromLocal8Bit(argv[1])=="--unavailable") {
        QTimer::singleShot(200,&app,[&]{ bus.unregisterService(service); });
        QTimer::singleShot(16000,&app,[&]{
            check(!recorder.recording(),"bounded recovery stops");
            check(recorder.error().contains("three recovery attempts"),"visible recovery failure");
            QFile file(temporary.path()+"/gps-recordings/"+recorder.fileName()); check(file.open(QIODevice::ReadOnly),"read recovery file");
            int attempts=0; QString end;
            while(!file.atEnd()) { auto e=QJsonDocument::fromJson(file.readLine()).object();
                if(e["event"]=="provider_recovery") ++attempts;
                if(e["event"]=="session_end") end=e["reason"].toString();
            }
            check(attempts==3 && end=="recovery_exhausted","three retries and explicit end marker");
            qInfo()<<"PASS: absent provider retried three times then stopped with error"; app.quit();
        });
        return app.exec();
    }
    QTimer::singleShot(200,&app,[&]{ provider.send(base+".Position","PositionChanged",position(0,QDateTime::currentSecsSinceEpoch())); });
    QTimer::singleShot(300,&app,[&]{ provider.send(base+".Position","PositionChanged",position(3,QDateTime::currentSecsSinceEpoch())); });
    QTimer::singleShot(350,&app,[&]{
        check(recorder.fixAge()>=0,"live signal accepted");
        provider.send(base+".Satellite","SatelliteChanged",satellites(QDateTime::currentSecsSinceEpoch()));
        provider.send(base+".Velocity","VelocityChanged",{3,int(QDateTime::currentSecsSinceEpoch()),2.5,123.0,nan()});
        provider.send(base,"StatusChanged",{3});
    });
    QTimer::singleShot(500,&app,[&]{ check(recorder.visible()==2 && recorder.used()==1 && recorder.signalCount()==1,"satellite UI decoding"); bus.unregisterService(service); });
    QTimer::singleShot(650,&app,[&]{ check(recorder.fixAge()==-1,"owner loss clears fix"); check(bus.registerService(service),"restart service"); });
    QTimer::singleShot(1000,&app,[&]{
        check(provider.refs==2,"provider restart reacquired");
        recorder.stop(); check(recorder.error().contains("synthetic velocity error"),"snapshot error surfaced");
        QFile f(temporary.path()+"/gps-recordings/"+recorder.fileName()); check(f.open(QIODevice::ReadOnly),"read recording");
        check(!(f.permissions()&(QFile::ReadGroup|QFile::ReadOther)),"private file permissions");
        bool invalid=false,fresh=false,cached=false,satellite=false,velocity=false,end=false,owner=false,last=false,dbusError=false;
        int sequence=0;
        while(!f.atEnd()) {
            QJsonParseError error; auto doc=QJsonDocument::fromJson(f.readLine(),&error); check(error.error==QJsonParseError::NoError,"valid JSONL");
            auto e=doc.object(); check(e["sequence"].toInt()==++sequence,"ordered sequence");
            check(e.contains("received_utc") && e.contains("boottime_ms"),"both clocks");
            auto args=e["arguments"].toArray(); auto member=e["member"].toString();
            if(member=="PositionChanged") {
                if(args[0].toInt()==0) invalid=args[2].toString()=="NaN" && !e["fresh_for_session"].toBool();
                else fresh=e["fresh_for_session"].toBool();
            }
            if(member=="GetPosition" && e["event"]=="dbus") cached=!e["fresh_for_session"].toBool() && args[5].toArray()[2].toString()=="NaN";
            if(member=="SatelliteChanged") satellite=args[3].toArray()[0].toInt()==64 && args[4].toArray()[1].toArray()==QJsonArray{64,40,50,22};
            if(member=="VelocityChanged") velocity=args[2].toDouble()==2.5 && args[4].toString()=="NaN";
            if(member=="GetLastSatellite" && e["event"]=="dbus") last=true;
            if(e["event"]=="provider_owner_changed") owner=true;
            if(e["event"]=="session_end") end=true;
            if(e["event"]=="dbus_error" && e["error_name"]=="org.freedesktop.Geoclue.Error.NotAvailable") dbusError=true;
        }
        check(invalid&&fresh&&cached&&satellite&&velocity&&end&&owner&&last&&dbusError,"full lossless event coverage");
        const auto first=recorder.fileName(); recorder.start(); recorder.stop(); check(first!=recorder.fileName(),"unique restart file");
        QFile blocker(temporary.path()+"/not-directory"); check(blocker.open(QIODevice::WriteOnly),"create blocker"); blocker.close();
        qputenv("XDG_DATA_HOME",blocker.fileName().toUtf8()); recorder.start();
        check(!recorder.recording()&&!recorder.error().isEmpty(),"directory failure visible and no acquisition");
        qInfo()<<"PASS: complete raw payloads, NaN, cached vs fresh, owner restart, D-Bus errors, lifecycle, private files, write setup failure";
        app.quit();
    });
    QTimer::singleShot(10000,&app,[]{qFatal("test timeout");});
    return app.exec();
}
